use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use evdev::{Device, EventType, KeyCode};
use tokio::sync::mpsc::UnboundedSender;

use crate::core::config::Shortcuts;
use crate::input::keymap;
use crate::input::uinput::VirtualKeyboard;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Binding {
    Primary,
    Secondary,
    Cancel,
    LongFormSubmit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    Pressed(Binding),
    Released(Binding),
}

#[derive(Debug, Clone)]
pub struct BindingSpec {
    pub binding: Binding,
    pub keys: Vec<KeyCode>,
}

pub fn resolve_bindings(shortcuts: &Shortcuts) -> (Vec<BindingSpec>, Vec<String>) {
    let mut specs = Vec::new();
    let mut problems = Vec::new();

    let add = |binding: Binding,
               chord: Option<&str>,
               specs: &mut Vec<BindingSpec>,
               problems: &mut Vec<String>| {
        let Some(chord) = chord else { return };
        if chord.trim().is_empty() {
            return;
        }
        match keymap::parse_chord(chord) {
            Ok(keys) => specs.push(BindingSpec { binding, keys }),
            Err(err) => problems.push(err),
        }
    };

    add(
        Binding::Primary,
        Some(shortcuts.primary.as_str()),
        &mut specs,
        &mut problems,
    );
    add(
        Binding::Secondary,
        shortcuts.secondary.as_deref(),
        &mut specs,
        &mut problems,
    );
    add(
        Binding::Cancel,
        shortcuts.cancel.as_deref(),
        &mut specs,
        &mut problems,
    );
    add(
        Binding::LongFormSubmit,
        shortcuts.long_form_submit.as_deref(),
        &mut specs,
        &mut problems,
    );

    specs.sort_by_key(|spec| std::cmp::Reverse(spec.keys.len()));
    (specs, problems)
}

#[derive(Clone, Copy)]
struct RawKey {
    key: KeyCode,
    value: i32,
    relay: bool,
}

struct Shared {
    stop: AtomicBool,
    grab: bool,
    passthrough: Option<Mutex<VirtualKeyboard>>,
}

pub struct HotkeyListener {
    shared: Arc<Shared>,
}

impl HotkeyListener {
    pub fn start(
        shortcuts: &Shortcuts,
        specs: Vec<BindingSpec>,
        events: UnboundedSender<HotkeyEvent>,
    ) -> Result<Self> {
        if specs.is_empty() {
            anyhow::bail!("no usable shortcuts configured");
        }

        let passthrough = if shortcuts.grab_keys {
            Some(Mutex::new(
                VirtualKeyboard::open(Duration::ZERO)
                    .context("creating the passthrough virtual keyboard")?,
            ))
        } else {
            None
        };

        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            grab: shortcuts.grab_keys,
            passthrough,
        });
        let devices: Arc<Mutex<HashMap<PathBuf, ()>>> = Arc::new(Mutex::new(HashMap::new()));

        let (raw_tx, raw_rx) = std::sync::mpsc::channel::<RawKey>();

        {
            let shared = Arc::clone(&shared);
            let specs = specs.clone();
            std::thread::Builder::new()
                .name("voxscribe-hotkeys".into())
                .spawn(move || chord_loop(shared, specs, raw_rx, events))
                .context("spawning the shortcut state machine")?;
        }

        let filter = DeviceFilter {
            names: shortcuts.device_names.clone(),
            path: shortcuts.device_path.clone(),
            required_keys: specs.iter().flat_map(|s| s.keys.clone()).collect(),
        };

        {
            let shared = Arc::clone(&shared);
            let hotplug = shortcuts.hotplug;
            std::thread::Builder::new()
                .name("voxscribe-kbd-scan".into())
                .spawn(move || scan_loop(shared, devices, filter, raw_tx, hotplug))
                .context("spawning the keyboard scanner")?;
        }

        Ok(Self { shared })
    }

    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::SeqCst);
    }
}

impl Drop for HotkeyListener {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Clone)]
struct DeviceFilter {
    names: Vec<String>,
    path: Option<PathBuf>,
    required_keys: HashSet<KeyCode>,
}

impl DeviceFilter {
    fn accepts(&self, path: &Path, device: &Device) -> bool {
        if let Some(wanted) = &self.path {
            return wanted == path;
        }

        let Some(supported) = device.supported_keys() else {
            return false;
        };
        if !self
            .required_keys
            .iter()
            .all(|key| supported.contains(*key))
        {
            return false;
        }

        if self.names.is_empty() {
            return true;
        }
        let name = device.name().unwrap_or_default().to_ascii_lowercase();
        self.names
            .iter()
            .any(|wanted| name.contains(&wanted.to_ascii_lowercase()))
    }
}

fn scan_loop(
    shared: Arc<Shared>,
    devices: Arc<Mutex<HashMap<PathBuf, ()>>>,
    filter: DeviceFilter,
    raw_tx: std::sync::mpsc::Sender<RawKey>,
    hotplug: bool,
) {
    loop {
        if shared.stop.load(Ordering::SeqCst) {
            return;
        }

        for (path, device) in enumerate_keyboards() {
            if shared.stop.load(Ordering::SeqCst) {
                return;
            }
            {
                let known = devices.lock().expect("device map poisoned");
                if known.contains_key(&path) {
                    continue;
                }
            }
            if !filter.accepts(&path, &device) {
                continue;
            }

            let name = device.name().unwrap_or("<unnamed>").to_string();
            devices
                .lock()
                .expect("device map poisoned")
                .insert(path.clone(), ());
            tracing::info!("listening on keyboard {name} ({})", path.display());

            let thread_shared = Arc::clone(&shared);
            let thread_devices = Arc::clone(&devices);
            let raw_tx = raw_tx.clone();
            let thread_path = path.clone();
            if let Err(err) = std::thread::Builder::new()
                .name("voxscribe-kbd".into())
                .spawn(move || {
                    reader_loop(thread_shared, device, &thread_path, raw_tx);
                    thread_devices
                        .lock()
                        .expect("device map poisoned")
                        .remove(&thread_path);
                })
            {
                tracing::warn!("could not watch {}: {err}", path.display());
                devices.lock().expect("device map poisoned").remove(&path);
            }
        }

        if !hotplug {
            return;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn enumerate_keyboards() -> Vec<(PathBuf, Device)> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir("/dev/input") else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("event"))
        {
            continue;
        }
        let Ok(device) = Device::open(&path) else {
            continue;
        };
        if device.name().is_some_and(|name| {
            let name = name.to_ascii_lowercase();
            name.starts_with("voxscribe ") || name.contains("ydotool") || name.contains("wtype")
        }) {
            continue;
        }
        if device.supported_events().contains(EventType::KEY) {
            found.push((path, device));
        }
    }
    found
}

fn reader_loop(
    shared: Arc<Shared>,
    mut device: Device,
    path: &Path,
    raw_tx: std::sync::mpsc::Sender<RawKey>,
) {
    let grabbed = if shared.grab {
        if let Err(err) = device.grab() {
            tracing::warn!(
                "could not grab {}: {err} (continuing ungrabbed)",
                path.display()
            );
            false
        } else {
            true
        }
    } else {
        false
    };

    loop {
        if shared.stop.load(Ordering::SeqCst) {
            break;
        }
        let events = match device.fetch_events() {
            Ok(events) => events,
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(err) => {
                tracing::info!("keyboard {} closed: {err}", path.display());
                break;
            }
        };

        for event in events {
            if event.event_type() == EventType::KEY {
                let _ = raw_tx.send(RawKey {
                    key: KeyCode::new(event.code()),
                    value: event.value(),
                    relay: grabbed,
                });
            } else if grabbed {
                if let Some(passthrough) = &shared.passthrough {
                    if let Ok(mut keyboard) = passthrough.lock() {
                        let _ =
                            keyboard.passthrough(event.event_type().0, event.code(), event.value());
                    }
                }
            }
        }
    }
}

fn chord_loop(
    shared: Arc<Shared>,
    specs: Vec<BindingSpec>,
    raw_rx: std::sync::mpsc::Receiver<RawKey>,
    events: UnboundedSender<HotkeyEvent>,
) {
    let mut matcher = ChordMatcher::new(specs.clone());
    let mut pending = Vec::new();
    let mut suppressed = HashSet::new();
    let mut bypassing = false;
    loop {
        if shared.stop.load(Ordering::SeqCst) {
            return;
        }
        let raw = match raw_rx.recv_timeout(Duration::from_millis(250)) {
            Ok(raw) => raw,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        };
        if !raw.relay {
            for event in matcher.feed(raw.key, raw.value) {
                if events.send(event).is_err() {
                    return;
                }
            }
            continue;
        }

        if bypassing {
            let matched = matcher.feed(raw.key, raw.value);
            relay_key(&shared, raw);
            bypassing = !matcher.pressed.is_empty();
            for event in matched {
                if events.send(event).is_err() {
                    return;
                }
            }
            continue;
        }

        let was_suppressed = suppressed.contains(&raw.key);
        if suppressed.is_empty() {
            pending.push(raw);
        }

        let matched = matcher.feed(raw.key, raw.value);
        if let Some(binding) = matched.iter().rev().find_map(|event| match event {
            HotkeyEvent::Pressed(binding) => Some(*binding),
            HotkeyEvent::Released(_) => None,
        }) {
            pending.clear();
            if let Some(spec) = specs.iter().find(|spec| spec.binding == binding) {
                suppressed.extend(spec.keys.iter().copied());
            }
        } else if suppressed.is_empty() && !could_complete(&specs, &matcher.pressed) {
            relay_pending(&shared, &mut pending);
            bypassing = !matcher.pressed.is_empty();
        } else if !suppressed.is_empty() && !was_suppressed {
            relay_key(&shared, raw);
        }

        if raw.value == 0 {
            suppressed.remove(&raw.key);
        }

        for event in matched {
            if events.send(event).is_err() {
                return;
            }
        }
    }
}

fn could_complete(specs: &[BindingSpec], pressed: &HashSet<KeyCode>) -> bool {
    !pressed.is_empty()
        && pressed.iter().any(|key| keymap::is_modifier(*key))
        && specs
            .iter()
            .any(|spec| pressed.iter().all(|key| spec.keys.contains(key)))
}

fn relay_pending(shared: &Shared, pending: &mut Vec<RawKey>) {
    for raw in pending.drain(..) {
        relay_key(shared, raw);
    }
}

fn relay_key(shared: &Shared, raw: RawKey) {
    if let Some(passthrough) = &shared.passthrough {
        if let Ok(mut keyboard) = passthrough.lock() {
            let _ = keyboard.passthrough(EventType::KEY.0, raw.key.code(), raw.value);
        }
    }
}

pub struct ChordMatcher {
    specs: Vec<BindingSpec>,
    pressed: HashSet<KeyCode>,
    active: Option<Binding>,
    last_trigger: Option<Instant>,
    debounce: Duration,
}

impl ChordMatcher {
    pub fn new(specs: Vec<BindingSpec>) -> Self {
        Self {
            specs,
            pressed: HashSet::new(),
            active: None,
            last_trigger: None,
            debounce: Duration::from_millis(0),
        }
    }

    pub fn with_debounce(mut self, debounce: Duration) -> Self {
        self.debounce = debounce;
        self
    }

    pub fn feed(&mut self, key: KeyCode, value: i32) -> Vec<HotkeyEvent> {
        match value {
            1 => {
                self.pressed.insert(key);
            }
            0 => {
                self.pressed.remove(&key);
            }
            _ => return Vec::new(),
        }

        let mut out = Vec::new();
        let matched = self.match_binding();

        match (self.active, matched) {
            (None, Some(binding)) => {
                if self.debounce_ok() {
                    self.active = Some(binding);
                    self.last_trigger = Some(Instant::now());
                    out.push(HotkeyEvent::Pressed(binding));
                }
            }
            (Some(active), None) => {
                self.active = None;
                out.push(HotkeyEvent::Released(active));
            }
            (Some(active), Some(binding)) if active != binding => {
                self.active = Some(binding);
                out.push(HotkeyEvent::Released(active));
                out.push(HotkeyEvent::Pressed(binding));
            }
            _ => {}
        }

        out
    }

    fn debounce_ok(&self) -> bool {
        match self.last_trigger {
            Some(at) => at.elapsed() >= self.debounce,
            None => true,
        }
    }

    fn match_binding(&self) -> Option<Binding> {
        for spec in &self.specs {
            if !spec.keys.iter().all(|key| self.pressed.contains(key)) {
                continue;
            }
            let extra_modifiers = self
                .pressed
                .iter()
                .filter(|key| keymap::is_modifier(**key) && !spec.keys.contains(key))
                .count();
            if extra_modifiers == 0 {
                return Some(spec.binding);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(binding: Binding, chord: &str) -> BindingSpec {
        BindingSpec {
            binding,
            keys: keymap::parse_chord(chord).unwrap(),
        }
    }

    #[test]
    fn a_chord_fires_once_on_completion_and_once_on_release() {
        let mut matcher = ChordMatcher::new(vec![spec(Binding::Primary, "SUPER+ALT+D")]);
        assert!(matcher.feed(KeyCode::KEY_LEFTMETA, 1).is_empty());
        assert!(matcher.feed(KeyCode::KEY_LEFTALT, 1).is_empty());
        assert_eq!(
            matcher.feed(KeyCode::KEY_D, 1),
            vec![HotkeyEvent::Pressed(Binding::Primary)]
        );
        assert_eq!(
            matcher.feed(KeyCode::KEY_D, 0),
            vec![HotkeyEvent::Released(Binding::Primary)]
        );
    }

    #[test]
    fn autorepeat_does_not_retrigger_a_held_chord() {
        let mut matcher = ChordMatcher::new(vec![spec(Binding::Primary, "f12")]);
        assert_eq!(matcher.feed(KeyCode::KEY_F12, 1).len(), 1);
        assert!(matcher.feed(KeyCode::KEY_F12, 2).is_empty());
        assert!(matcher.feed(KeyCode::KEY_F12, 2).is_empty());
    }

    #[test]
    fn an_extra_modifier_suppresses_the_match() {
        let mut matcher = ChordMatcher::new(vec![spec(Binding::Primary, "SUPER+D")]);
        matcher.feed(KeyCode::KEY_LEFTSHIFT, 1);
        matcher.feed(KeyCode::KEY_LEFTMETA, 1);
        assert!(matcher.feed(KeyCode::KEY_D, 1).is_empty());
    }

    #[test]
    fn the_longest_matching_chord_wins() {
        let (specs, problems) = resolve_bindings(&Shortcuts {
            primary: "SUPER+ALT+D".into(),
            cancel: Some("SUPER+D".into()),
            ..Shortcuts::default()
        });
        assert!(problems.is_empty());
        let mut matcher = ChordMatcher::new(specs);
        matcher.feed(KeyCode::KEY_LEFTMETA, 1);
        matcher.feed(KeyCode::KEY_LEFTALT, 1);
        assert_eq!(
            matcher.feed(KeyCode::KEY_D, 1),
            vec![HotkeyEvent::Pressed(Binding::Primary)]
        );
    }

    #[test]
    fn releasing_a_modifier_downgrades_to_the_shorter_chord() {
        let (specs, _) = resolve_bindings(&Shortcuts {
            primary: "SUPER+ALT+D".into(),
            cancel: Some("SUPER+D".into()),
            ..Shortcuts::default()
        });
        let mut matcher = ChordMatcher::new(specs);
        matcher.feed(KeyCode::KEY_LEFTMETA, 1);
        matcher.feed(KeyCode::KEY_LEFTALT, 1);
        matcher.feed(KeyCode::KEY_D, 1);
        assert_eq!(
            matcher.feed(KeyCode::KEY_LEFTALT, 0),
            vec![
                HotkeyEvent::Released(Binding::Primary),
                HotkeyEvent::Pressed(Binding::Cancel),
            ]
        );
    }

    #[test]
    fn typing_other_keys_while_held_does_not_release_the_chord() {
        let mut matcher = ChordMatcher::new(vec![spec(Binding::Primary, "f12")]);
        matcher.feed(KeyCode::KEY_F12, 1);
        assert!(matcher.feed(KeyCode::KEY_A, 1).is_empty());
        assert!(matcher.feed(KeyCode::KEY_A, 0).is_empty());
        assert_eq!(matcher.feed(KeyCode::KEY_F12, 0).len(), 1);
    }

    #[test]
    fn unparsable_shortcuts_are_reported_and_the_rest_still_bind() {
        let (specs, problems) = resolve_bindings(&Shortcuts {
            primary: "SUPER+ALT+D".into(),
            cancel: Some("SUPER+NOPE".into()),
            ..Shortcuts::default()
        });
        assert_eq!(specs.len(), 1);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("NOPE"));
    }

    #[test]
    fn debounce_rejects_a_retrigger_inside_the_window() {
        let mut matcher = ChordMatcher::new(vec![spec(Binding::Primary, "f12")])
            .with_debounce(Duration::from_secs(30));
        assert_eq!(matcher.feed(KeyCode::KEY_F12, 1).len(), 1);
        matcher.feed(KeyCode::KEY_F12, 0);
        assert!(matcher.feed(KeyCode::KEY_F12, 1).is_empty());
    }

    #[test]
    fn only_modifier_led_prefixes_are_buffered() {
        let specs = vec![spec(Binding::Primary, "SUPER+D")];
        assert!(!could_complete(&specs, &HashSet::from([KeyCode::KEY_D])));
        assert!(could_complete(
            &specs,
            &HashSet::from([KeyCode::KEY_LEFTMETA])
        ));
    }
}
