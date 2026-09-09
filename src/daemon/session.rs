use std::time::Duration;

use crate::core::config::RecordingMode;
use crate::input::hotkeys::Binding;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Nothing,
    Start { language: Option<String> },
    Stop,
    Cancel,
    Submit,
    Pause,
    Resume,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    Idle,
    Recording,
    Paused,
}

#[derive(Debug, Clone)]
pub struct SessionRules {
    pub mode: RecordingMode,
    pub tap_threshold: Duration,
    pub secondary_language: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AutoModeState {
    pub latched: bool,
    pub started_this_press: bool,
}

pub fn on_press(
    rules: &SessionRules,
    phase: SessionPhase,
    binding: Binding,
    auto: &mut AutoModeState,
) -> Action {
    match binding {
        Binding::Cancel => {
            return if phase == SessionPhase::Idle {
                Action::Nothing
            } else {
                Action::Cancel
            };
        }
        Binding::LongFormSubmit => {
            return if phase == SessionPhase::Idle {
                Action::Nothing
            } else {
                Action::Submit
            };
        }
        Binding::Primary | Binding::Secondary => {}
    }

    let language = if binding == Binding::Secondary {
        rules.secondary_language.clone()
    } else {
        None
    };

    match rules.mode {
        RecordingMode::PushToTalk => match phase {
            SessionPhase::Idle => Action::Start { language },
            _ => Action::Nothing,
        },
        RecordingMode::Toggle | RecordingMode::Continuous => match phase {
            SessionPhase::Idle => Action::Start { language },
            _ => Action::Stop,
        },
        RecordingMode::Auto => match phase {
            SessionPhase::Idle => {
                auto.started_this_press = true;
                auto.latched = false;
                Action::Start { language }
            }
            _ => {
                auto.started_this_press = false;
                if auto.latched {
                    auto.latched = false;
                    Action::Stop
                } else {
                    Action::Nothing
                }
            }
        },
        RecordingMode::LongForm => match phase {
            SessionPhase::Idle => Action::Start { language },
            SessionPhase::Recording => Action::Pause,
            SessionPhase::Paused => Action::Resume,
        },
    }
}

pub fn on_release(
    rules: &SessionRules,
    phase: SessionPhase,
    binding: Binding,
    held: Duration,
    auto: &mut AutoModeState,
) -> Action {
    if !matches!(binding, Binding::Primary | Binding::Secondary) {
        return Action::Nothing;
    }

    match rules.mode {
        RecordingMode::PushToTalk => match phase {
            SessionPhase::Recording => Action::Stop,
            _ => Action::Nothing,
        },
        RecordingMode::Auto => {
            if !auto.started_this_press || phase != SessionPhase::Recording {
                return Action::Nothing;
            }
            auto.started_this_press = false;
            if held < rules.tap_threshold {
                auto.latched = true;
                Action::Nothing
            } else {
                Action::Stop
            }
        }
        RecordingMode::Toggle | RecordingMode::Continuous | RecordingMode::LongForm => {
            Action::Nothing
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(mode: RecordingMode) -> SessionRules {
        SessionRules {
            mode,
            tap_threshold: Duration::from_millis(400),
            secondary_language: Some("it".into()),
        }
    }

    fn press(mode: RecordingMode, phase: SessionPhase, auto: &mut AutoModeState) -> Action {
        on_press(&rules(mode), phase, Binding::Primary, auto)
    }

    #[test]
    fn push_to_talk_records_only_while_held() {
        let mut auto = AutoModeState::default();
        assert_eq!(
            press(RecordingMode::PushToTalk, SessionPhase::Idle, &mut auto),
            Action::Start { language: None }
        );
        assert_eq!(
            on_release(
                &rules(RecordingMode::PushToTalk),
                SessionPhase::Recording,
                Binding::Primary,
                Duration::from_secs(2),
                &mut auto,
            ),
            Action::Stop
        );
    }

    #[test]
    fn toggle_starts_on_one_press_and_stops_on_the_next() {
        let mut auto = AutoModeState::default();
        assert_eq!(
            press(RecordingMode::Toggle, SessionPhase::Idle, &mut auto),
            Action::Start { language: None }
        );
        assert_eq!(
            press(RecordingMode::Toggle, SessionPhase::Recording, &mut auto),
            Action::Stop
        );
    }

    #[test]
    fn toggle_ignores_the_release() {
        let mut auto = AutoModeState::default();
        assert_eq!(
            on_release(
                &rules(RecordingMode::Toggle),
                SessionPhase::Recording,
                Binding::Primary,
                Duration::from_millis(50),
                &mut auto,
            ),
            Action::Nothing
        );
    }

    #[test]
    fn auto_mode_treats_a_quick_tap_as_a_toggle() {
        let mut auto = AutoModeState::default();
        assert_eq!(
            press(RecordingMode::Auto, SessionPhase::Idle, &mut auto),
            Action::Start { language: None }
        );
        assert_eq!(
            on_release(
                &rules(RecordingMode::Auto),
                SessionPhase::Recording,
                Binding::Primary,
                Duration::from_millis(100),
                &mut auto,
            ),
            Action::Nothing
        );
        assert!(auto.latched);
        assert_eq!(
            press(RecordingMode::Auto, SessionPhase::Recording, &mut auto),
            Action::Stop
        );
    }

    #[test]
    fn auto_mode_treats_a_long_hold_as_push_to_talk() {
        let mut auto = AutoModeState::default();
        press(RecordingMode::Auto, SessionPhase::Idle, &mut auto);
        assert_eq!(
            on_release(
                &rules(RecordingMode::Auto),
                SessionPhase::Recording,
                Binding::Primary,
                Duration::from_millis(900),
                &mut auto,
            ),
            Action::Stop
        );
        assert!(!auto.latched);
    }

    #[test]
    fn auto_mode_ignores_the_release_that_ends_a_stopping_press() {
        let mut auto = AutoModeState::default();
        press(RecordingMode::Auto, SessionPhase::Idle, &mut auto);
        on_release(
            &rules(RecordingMode::Auto),
            SessionPhase::Recording,
            Binding::Primary,
            Duration::from_millis(100),
            &mut auto,
        );
        assert_eq!(
            press(RecordingMode::Auto, SessionPhase::Recording, &mut auto),
            Action::Stop
        );
        assert_eq!(
            on_release(
                &rules(RecordingMode::Auto),
                SessionPhase::Idle,
                Binding::Primary,
                Duration::from_millis(80),
                &mut auto,
            ),
            Action::Nothing
        );
    }

    #[test]
    fn long_form_cycles_between_recording_and_paused() {
        let mut auto = AutoModeState::default();
        assert_eq!(
            press(RecordingMode::LongForm, SessionPhase::Idle, &mut auto),
            Action::Start { language: None }
        );
        assert_eq!(
            press(RecordingMode::LongForm, SessionPhase::Recording, &mut auto),
            Action::Pause
        );
        assert_eq!(
            press(RecordingMode::LongForm, SessionPhase::Paused, &mut auto),
            Action::Resume
        );
    }

    #[test]
    fn the_submit_shortcut_finishes_a_long_form_recording_from_either_state() {
        let mut auto = AutoModeState::default();
        for phase in [SessionPhase::Recording, SessionPhase::Paused] {
            assert_eq!(
                on_press(
                    &rules(RecordingMode::LongForm),
                    phase,
                    Binding::LongFormSubmit,
                    &mut auto
                ),
                Action::Submit
            );
        }
        assert_eq!(
            on_press(
                &rules(RecordingMode::LongForm),
                SessionPhase::Idle,
                Binding::LongFormSubmit,
                &mut auto
            ),
            Action::Nothing
        );
    }

    #[test]
    fn cancel_discards_from_any_active_state_and_does_nothing_when_idle() {
        let mut auto = AutoModeState::default();
        for mode in [
            RecordingMode::Toggle,
            RecordingMode::LongForm,
            RecordingMode::PushToTalk,
        ] {
            assert_eq!(
                on_press(
                    &rules(mode),
                    SessionPhase::Recording,
                    Binding::Cancel,
                    &mut auto
                ),
                Action::Cancel
            );
            assert_eq!(
                on_press(&rules(mode), SessionPhase::Idle, Binding::Cancel, &mut auto),
                Action::Nothing
            );
        }
    }

    #[test]
    fn the_secondary_shortcut_carries_its_configured_language() {
        let mut auto = AutoModeState::default();
        assert_eq!(
            on_press(
                &rules(RecordingMode::Toggle),
                SessionPhase::Idle,
                Binding::Secondary,
                &mut auto
            ),
            Action::Start {
                language: Some("it".into())
            }
        );
    }

    #[test]
    fn continuous_mode_starts_and_stops_like_toggle() {
        let mut auto = AutoModeState::default();
        assert_eq!(
            press(RecordingMode::Continuous, SessionPhase::Idle, &mut auto),
            Action::Start { language: None }
        );
        assert_eq!(
            press(
                RecordingMode::Continuous,
                SessionPhase::Recording,
                &mut auto
            ),
            Action::Stop
        );
    }
}
