//! Persistent `/dev/uinput` virtual keyboard, created once at daemon start -
//! compositors need a moment to notice a new keyboard, so a per-dictation
//! device would drop the first characters. Real evdev events, so no wtype or
//! ydotool.

use std::io;
use std::time::Duration;

use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, EventType, InputEvent, KeyCode};

const KEY_DOWN: i32 = 1;
const KEY_UP: i32 = 0;

pub struct VirtualKeyboard {
    device: VirtualDevice,
    /// Rate-limited consumers drop characters without this.
    key_delay: Duration,
}

impl VirtualKeyboard {
    pub fn open(key_delay: Duration) -> io::Result<Self> {
        // This device re-emits for grabbed keyboards, so it must be able to
        // express any key they send.
        let mut keys = AttributeSet::<KeyCode>::new();
        for code in 1u16..=248 {
            keys.insert(KeyCode::new(code));
        }

        let device = VirtualDevice::builder()?
            .name("Duskr Virtual Keyboard")
            .with_keys(&keys)?
            .build()?;

        Ok(Self { device, key_delay })
    }

    pub fn set_key_delay(&mut self, delay: Duration) {
        self.key_delay = delay;
    }

    fn emit(&mut self, events: &[InputEvent]) -> io::Result<()> {
        self.device.emit(events)
    }

    fn key(&mut self, key: KeyCode, value: i32) -> io::Result<()> {
        self.emit(&[InputEvent::new(EventType::KEY.0, key.code(), value)])
    }

    /// Re-emit verbatim, so grabbed keyboards still reach the compositor.
    pub fn passthrough(&mut self, event_type: u16, code: u16, value: i32) -> io::Result<()> {
        self.emit(&[InputEvent::new(event_type, code, value)])
    }

    pub fn tap(&mut self, key: KeyCode) -> io::Result<()> {
        self.key(key, KEY_DOWN)?;
        self.key(key, KEY_UP)
    }

    /// Press `chord` in order, then release in reverse order.
    pub fn chord(&mut self, keys: &[KeyCode]) -> io::Result<()> {
        for key in keys {
            self.key(*key, KEY_DOWN)?;
            spin(self.key_delay);
        }
        for key in keys.iter().rev() {
            self.key(*key, KEY_UP)?;
            spin(self.key_delay);
        }
        Ok(())
    }

    /// Returns the count emitted. Characters with no US-layout keystroke are
    /// skipped for the caller to route through the clipboard.
    pub fn type_text(&mut self, text: &str) -> io::Result<usize> {
        let mut typed = 0usize;
        let mut shift_held = false;

        for c in text.chars() {
            let Some((key, needs_shift)) = super::keymap::char_to_key(c) else {
                continue;
            };

            // Toggle shift only on change, not three events per character.
            if needs_shift != shift_held {
                self.key(
                    KeyCode::KEY_LEFTSHIFT,
                    if needs_shift { KEY_DOWN } else { KEY_UP },
                )?;
                shift_held = needs_shift;
                spin(self.key_delay);
            }

            self.key(key, KEY_DOWN)?;
            spin(self.key_delay);
            self.key(key, KEY_UP)?;
            spin(self.key_delay);
            typed += 1;
        }

        if shift_held {
            self.key(KeyCode::KEY_LEFTSHIFT, KEY_UP)?;
        }

        Ok(typed)
    }

    /// Called before injecting, so a held push-to-talk chord cannot turn the
    /// transcript into shortcuts.
    pub fn release_modifiers(&mut self) -> io::Result<()> {
        for key in super::keymap::MODIFIERS {
            self.key(*key, KEY_UP)?;
        }
        Ok(())
    }
}

fn spin(delay: Duration) {
    if !delay.is_zero() {
        std::thread::sleep(delay);
    }
}

/// Why `/dev/uinput` is unusable, in terms the user can act on.
pub fn diagnose() -> Result<(), String> {
    let path = std::path::Path::new("/dev/uinput");
    if !path.exists() {
        return Err("/dev/uinput is missing - load the uinput kernel module \
                    (`sudo modprobe uinput`)"
            .into());
    }
    match std::fs::OpenOptions::new().write(true).open(path) {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::PermissionDenied => Err(
            "/dev/uinput is not writable - add your user to the `input` group \
             (`sudo usermod -aG input $USER`) and log back in"
                .into(),
        ),
        Err(err) => Err(format!("/dev/uinput could not be opened: {err}")),
    }
}
