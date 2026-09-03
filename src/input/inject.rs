//! Delivering a transcript to whatever window has focus.
//!
//! The default path synthesizes real key events on the persistent virtual
//! keyboard, which every consumer understands - games with raw input, Discord,
//! browsers, terminals, XWayland. The clipboard is used only when a transcript
//! contains characters no US-layout keystroke can produce, and the previous
//! selection is restored afterwards.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use evdev::KeyCode;

use crate::core::config::{AppRule, Config, InjectMode};
use crate::input::{clipboard, keymap, uinput::VirtualKeyboard, window};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectOutcome {
    pub mode: InjectMode,
    pub submitted: bool,
    pub chars: usize,
    /// Characters that had to take the clipboard path, if any.
    pub fallback_chars: Vec<char>,
}

/// The settings that actually apply, after the per-application rule is merged
/// over the globals.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveRule {
    pub mode: InjectMode,
    pub auto_submit: bool,
    pub paste_chord: String,
    pub key_delay_us: u64,
    pub matched: Option<String>,
}

/// Merge the global input settings with the first matching application rule.
pub fn effective_rule(config: &Config, identifiers: &[String]) -> EffectiveRule {
    let mut rule = EffectiveRule {
        mode: config.input.mode,
        auto_submit: config.general.auto_submit,
        paste_chord: config.input.paste_chord.clone(),
        key_delay_us: config.input.key_delay_us,
        matched: None,
    };

    let Some((ident, app)) = identifiers.iter().find_map(|ident| {
        config
            .input
            .applications
            .get(ident)
            .map(|rule| (ident, rule))
    }) else {
        return rule;
    };

    rule.matched = Some(ident.clone());
    apply_app_rule(&mut rule, app);
    rule
}

fn apply_app_rule(rule: &mut EffectiveRule, app: &AppRule) {
    if app.disabled {
        rule.mode = InjectMode::None;
        rule.auto_submit = false;
        return;
    }
    if let Some(mode) = app.mode {
        rule.mode = mode;
    }
    if let Some(auto_submit) = app.auto_submit {
        rule.auto_submit = auto_submit;
    }
    if let Some(chord) = &app.paste_chord {
        rule.paste_chord = chord.clone();
    }
    if let Some(delay) = app.key_delay_us {
        rule.key_delay_us = delay;
    }
}

/// Which delivery path a transcript takes under a given rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    Nothing,
    Type,
    /// Clipboard paste; carries the characters that forced the decision so the
    /// caller can explain itself in logs.
    Paste {
        forced_by: Vec<char>,
    },
}

pub fn plan(text: &str, mode: InjectMode) -> Plan {
    if text.is_empty() {
        return Plan::Nothing;
    }
    match mode {
        InjectMode::None => Plan::Nothing,
        InjectMode::Type => Plan::Type,
        InjectMode::Clipboard => Plan::Paste {
            forced_by: Vec::new(),
        },
        InjectMode::Auto => {
            let untypable = keymap::untypable_chars(text);
            if untypable.is_empty() {
                Plan::Type
            } else {
                Plan::Paste {
                    forced_by: untypable,
                }
            }
        }
    }
}

pub struct Injector {
    keyboard: Arc<Mutex<VirtualKeyboard>>,
    restore_clipboard: bool,
    restore_delay: Duration,
    submit_delay: Duration,
}

impl Injector {
    pub fn new(config: &Config) -> Result<Self> {
        if let Err(reason) = crate::input::uinput::diagnose() {
            anyhow::bail!(reason);
        }
        let keyboard = VirtualKeyboard::open(Duration::from_micros(config.input.key_delay_us))
            .context("creating the Duskr virtual keyboard")?;
        Ok(Self {
            keyboard: Arc::new(Mutex::new(keyboard)),
            restore_clipboard: config.input.restore_clipboard,
            restore_delay: Duration::from_millis(config.input.restore_clipboard_delay_ms),
            submit_delay: Duration::from_millis(config.input.submit_delay_ms),
        })
    }

    pub fn reconfigure(&mut self, config: &Config) {
        self.restore_clipboard = config.input.restore_clipboard;
        self.restore_delay = Duration::from_millis(config.input.restore_clipboard_delay_ms);
        self.submit_delay = Duration::from_millis(config.input.submit_delay_ms);
    }

    /// Look up the focused window and inject `text` under the matching rule.
    ///
    /// Blocking: key events are paced with sleeps. Callers on the async runtime
    /// must wrap this in `spawn_blocking`.
    pub fn inject(&self, text: &str, config: &Config) -> Result<InjectOutcome> {
        let identifiers = window::focused()
            .map(|w| w.identifiers())
            .unwrap_or_default();
        let rule = effective_rule(config, &identifiers);
        self.inject_with(text, &rule)
    }

    pub fn inject_with(&self, text: &str, rule: &EffectiveRule) -> Result<InjectOutcome> {
        let plan = plan(text, rule.mode);
        if matches!(plan, Plan::Nothing) {
            return Ok(InjectOutcome {
                mode: rule.mode,
                submitted: false,
                chars: 0,
                fallback_chars: Vec::new(),
            });
        }

        {
            let mut keyboard = self.keyboard.lock().expect("virtual keyboard poisoned");
            keyboard.set_key_delay(Duration::from_micros(rule.key_delay_us));
            // A held push-to-talk chord would otherwise turn every typed
            // character into a shortcut.
            keyboard
                .release_modifiers()
                .context("releasing modifiers before injection")?;
        }

        let outcome = match plan {
            Plan::Nothing => unreachable!("handled above"),
            Plan::Type => {
                let chars = self
                    .keyboard
                    .lock()
                    .expect("virtual keyboard poisoned")
                    .type_text(text)
                    .context("typing the transcript")?;
                InjectOutcome {
                    mode: InjectMode::Type,
                    submitted: false,
                    chars,
                    fallback_chars: Vec::new(),
                }
            }
            Plan::Paste { forced_by } => {
                self.paste(text, rule)?;
                InjectOutcome {
                    mode: InjectMode::Clipboard,
                    submitted: false,
                    chars: text.chars().count(),
                    fallback_chars: forced_by,
                }
            }
        };

        let submitted = if rule.auto_submit {
            std::thread::sleep(self.submit_delay);
            self.keyboard
                .lock()
                .expect("virtual keyboard poisoned")
                .tap(KeyCode::KEY_ENTER)
                .context("sending Enter for auto-submit")?;
            true
        } else {
            false
        };

        Ok(InjectOutcome {
            submitted,
            ..outcome
        })
    }

    fn paste(&self, text: &str, rule: &EffectiveRule) -> Result<()> {
        let previous = if self.restore_clipboard {
            match clipboard::read() {
                Ok(previous) => previous,
                Err(err) => {
                    tracing::warn!("could not read the clipboard before pasting: {err}");
                    None
                }
            }
        } else {
            None
        };

        // Ownership has to outlive the paste keystroke, so the source is served
        // for a bounded window rather than a single roundtrip.
        let owner = clipboard::set_for(
            clipboard::Selection::text(text),
            self.restore_delay + Duration::from_secs(2),
        )
        .context("placing the transcript on the clipboard")?;

        // Give the compositor a moment to publish the new selection before the
        // paste chord asks for it.
        std::thread::sleep(Duration::from_millis(60));

        let chord =
            keymap::parse_chord(&rule.paste_chord).map_err(|err| anyhow::anyhow!("{err}"))?;
        self.keyboard
            .lock()
            .expect("virtual keyboard poisoned")
            .chord(&chord)
            .context("sending the paste chord")?;

        std::thread::sleep(self.restore_delay);
        owner.release();

        if let Some(previous) = previous {
            // Re-offering keeps the user's clipboard exactly as it was; without
            // this the transcript would silently replace it.
            match clipboard::set_for(previous, Duration::from_secs(3600)) {
                Ok(owner) => std::mem::forget(owner),
                Err(err) => tracing::warn!("could not restore the clipboard: {err}"),
            }
        }

        Ok(())
    }

    /// Send Enter on its own, for `duskr submit`-style flows.
    pub fn submit(&self) -> Result<()> {
        self.keyboard
            .lock()
            .expect("virtual keyboard poisoned")
            .tap(KeyCode::KEY_ENTER)
            .context("sending Enter")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::AppRule;

    fn config() -> Config {
        Config::default()
    }

    #[test]
    fn plain_ascii_is_typed_in_auto_mode() {
        assert_eq!(plan("hello world", InjectMode::Auto), Plan::Type);
    }

    #[test]
    fn auto_mode_falls_back_to_paste_only_for_untypable_characters() {
        assert_eq!(
            plan("café", InjectMode::Auto),
            Plan::Paste {
                forced_by: vec!['é']
            }
        );
    }

    #[test]
    fn clipboard_mode_always_pastes_and_type_mode_never_does() {
        assert_eq!(plan("café", InjectMode::Type), Plan::Type);
        assert_eq!(
            plan("hello", InjectMode::Clipboard),
            Plan::Paste {
                forced_by: Vec::new()
            }
        );
    }

    #[test]
    fn empty_text_and_none_mode_inject_nothing() {
        assert_eq!(plan("", InjectMode::Auto), Plan::Nothing);
        assert_eq!(plan("hello", InjectMode::None), Plan::Nothing);
    }

    #[test]
    fn global_settings_apply_when_no_application_rule_matches() {
        let config = config();
        let rule = effective_rule(&config, &["unknown-app".into()]);
        assert_eq!(rule.mode, config.input.mode);
        assert_eq!(rule.auto_submit, config.general.auto_submit);
        assert!(rule.matched.is_none());
    }

    #[test]
    fn an_application_rule_overrides_only_the_fields_it_sets() {
        let mut config = config();
        config.input.applications.insert(
            "discord".into(),
            AppRule {
                auto_submit: Some(true),
                ..AppRule::default()
            },
        );
        let rule = effective_rule(&config, &["discord".into()]);
        assert!(rule.auto_submit);
        assert_eq!(rule.mode, config.input.mode);
        assert_eq!(rule.matched.as_deref(), Some("discord"));
    }

    #[test]
    fn a_disabled_application_suppresses_injection_and_submission() {
        let mut config = config();
        config.general.auto_submit = true;
        config.input.applications.insert(
            "keepassxc".into(),
            AppRule {
                disabled: true,
                auto_submit: Some(true),
                ..AppRule::default()
            },
        );
        let rule = effective_rule(&config, &["keepassxc".into()]);
        assert_eq!(rule.mode, InjectMode::None);
        assert!(!rule.auto_submit);
        assert_eq!(plan("secret", rule.mode), Plan::Nothing);
    }

    #[test]
    fn the_first_matching_identifier_wins() {
        let mut config = config();
        config.input.applications.insert(
            "kitty".into(),
            AppRule {
                paste_chord: Some("ctrl+shift+v".into()),
                ..AppRule::default()
            },
        );
        config.input.applications.insert(
            "zsh".into(),
            AppRule {
                paste_chord: Some("ctrl+y".into()),
                ..AppRule::default()
            },
        );
        let rule = effective_rule(&config, &["kitty".into(), "zsh".into()]);
        assert_eq!(rule.paste_chord, "ctrl+shift+v");
    }
}
