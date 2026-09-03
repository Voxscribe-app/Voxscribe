//! Key-name and character tables shared by the hotkey listener and the
//! virtual keyboard.

use std::collections::HashMap;
use std::sync::OnceLock;

use evdev::KeyCode;

/// Modifiers, ignored when deciding whether an "extra" key is held down.
pub const MODIFIERS: &[KeyCode] = &[
    KeyCode::KEY_LEFTCTRL,
    KeyCode::KEY_RIGHTCTRL,
    KeyCode::KEY_LEFTSHIFT,
    KeyCode::KEY_RIGHTSHIFT,
    KeyCode::KEY_LEFTALT,
    KeyCode::KEY_RIGHTALT,
    KeyCode::KEY_LEFTMETA,
    KeyCode::KEY_RIGHTMETA,
];

pub fn is_modifier(key: KeyCode) -> bool {
    MODIFIERS.contains(&key)
}

fn aliases() -> &'static HashMap<&'static str, KeyCode> {
    static CACHE: OnceLock<HashMap<&'static str, KeyCode>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let mut map = HashMap::new();
        let mut add = |names: &[&'static str], key: KeyCode| {
            for name in names {
                map.insert(*name, key);
            }
        };

        add(
            &["ctrl", "control", "lctrl", "leftctrl"],
            KeyCode::KEY_LEFTCTRL,
        );
        add(&["rctrl", "rightctrl"], KeyCode::KEY_RIGHTCTRL);
        add(&["alt", "lalt", "leftalt"], KeyCode::KEY_LEFTALT);
        add(&["ralt", "rightalt", "altgr"], KeyCode::KEY_RIGHTALT);
        add(&["shift", "lshift", "leftshift"], KeyCode::KEY_LEFTSHIFT);
        add(&["rshift", "rightshift"], KeyCode::KEY_RIGHTSHIFT);
        add(
            &[
                "super", "meta", "win", "windows", "cmd", "lsuper", "leftmeta", "mod4",
            ],
            KeyCode::KEY_LEFTMETA,
        );
        add(
            &["rsuper", "rightsuper", "rmeta", "rightmeta"],
            KeyCode::KEY_RIGHTMETA,
        );

        add(&["enter", "return", "cr"], KeyCode::KEY_ENTER);
        add(&["kpenter"], KeyCode::KEY_KPENTER);
        add(&["backspace", "bksp"], KeyCode::KEY_BACKSPACE);
        add(&["tab"], KeyCode::KEY_TAB);
        add(&["caps", "capslock"], KeyCode::KEY_CAPSLOCK);
        add(&["esc", "escape"], KeyCode::KEY_ESC);
        add(&["space", "spacebar"], KeyCode::KEY_SPACE);
        add(&["delete", "del"], KeyCode::KEY_DELETE);
        add(&["insert", "ins"], KeyCode::KEY_INSERT);
        add(&["home"], KeyCode::KEY_HOME);
        add(&["end"], KeyCode::KEY_END);
        add(&["pageup", "pgup"], KeyCode::KEY_PAGEUP);
        add(&["pagedown", "pgdn", "pgdown"], KeyCode::KEY_PAGEDOWN);
        add(&["up", "uparrow"], KeyCode::KEY_UP);
        add(&["down", "downarrow"], KeyCode::KEY_DOWN);
        add(&["left", "leftarrow"], KeyCode::KEY_LEFT);
        add(&["right", "rightarrow"], KeyCode::KEY_RIGHT);
        add(&["menu"], KeyCode::KEY_MENU);
        add(
            &["print", "printscreen", "prtsc", "sysrq"],
            KeyCode::KEY_SYSRQ,
        );
        add(&["pause", "break"], KeyCode::KEY_PAUSE);
        add(&["numlock"], KeyCode::KEY_NUMLOCK);
        add(&["scrolllock", "scroll"], KeyCode::KEY_SCROLLLOCK);
        add(&["mute", "volumemute"], KeyCode::KEY_MUTE);
        add(&["volumeup", "volup"], KeyCode::KEY_VOLUMEUP);
        add(&["volumedown", "voldown"], KeyCode::KEY_VOLUMEDOWN);
        add(&["play", "playpause"], KeyCode::KEY_PLAYPAUSE);

        add(&[".", "dot", "period"], KeyCode::KEY_DOT);
        add(&[",", "comma"], KeyCode::KEY_COMMA);
        add(&["/", "slash"], KeyCode::KEY_SLASH);
        add(&["\\", "backslash"], KeyCode::KEY_BACKSLASH);
        add(&[";", "semicolon"], KeyCode::KEY_SEMICOLON);
        add(&["'", "apostrophe", "quote"], KeyCode::KEY_APOSTROPHE);
        add(&["[", "leftbrace", "lbrace"], KeyCode::KEY_LEFTBRACE);
        add(&["]", "rightbrace", "rbrace"], KeyCode::KEY_RIGHTBRACE);
        add(&["-", "minus", "dash"], KeyCode::KEY_MINUS);
        add(&["=", "equal", "equals"], KeyCode::KEY_EQUAL);
        add(&["`", "grave", "backtick"], KeyCode::KEY_GRAVE);

        const FKEYS: [KeyCode; 24] = [
            KeyCode::KEY_F1,
            KeyCode::KEY_F2,
            KeyCode::KEY_F3,
            KeyCode::KEY_F4,
            KeyCode::KEY_F5,
            KeyCode::KEY_F6,
            KeyCode::KEY_F7,
            KeyCode::KEY_F8,
            KeyCode::KEY_F9,
            KeyCode::KEY_F10,
            KeyCode::KEY_F11,
            KeyCode::KEY_F12,
            KeyCode::KEY_F13,
            KeyCode::KEY_F14,
            KeyCode::KEY_F15,
            KeyCode::KEY_F16,
            KeyCode::KEY_F17,
            KeyCode::KEY_F18,
            KeyCode::KEY_F19,
            KeyCode::KEY_F20,
            KeyCode::KEY_F21,
            KeyCode::KEY_F22,
            KeyCode::KEY_F23,
            KeyCode::KEY_F24,
        ];
        const FNAMES: [&str; 24] = [
            "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12", "f13",
            "f14", "f15", "f16", "f17", "f18", "f19", "f20", "f21", "f22", "f23", "f24",
        ];
        for (name, key) in FNAMES.iter().zip(FKEYS) {
            map.insert(*name, key);
        }

        const DIGITS: [KeyCode; 10] = [
            KeyCode::KEY_0,
            KeyCode::KEY_1,
            KeyCode::KEY_2,
            KeyCode::KEY_3,
            KeyCode::KEY_4,
            KeyCode::KEY_5,
            KeyCode::KEY_6,
            KeyCode::KEY_7,
            KeyCode::KEY_8,
            KeyCode::KEY_9,
        ];
        for (name, key) in ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"]
            .iter()
            .zip(DIGITS)
        {
            map.insert(*name, key);
        }

        const LETTERS: [KeyCode; 26] = [
            KeyCode::KEY_A,
            KeyCode::KEY_B,
            KeyCode::KEY_C,
            KeyCode::KEY_D,
            KeyCode::KEY_E,
            KeyCode::KEY_F,
            KeyCode::KEY_G,
            KeyCode::KEY_H,
            KeyCode::KEY_I,
            KeyCode::KEY_J,
            KeyCode::KEY_K,
            KeyCode::KEY_L,
            KeyCode::KEY_M,
            KeyCode::KEY_N,
            KeyCode::KEY_O,
            KeyCode::KEY_P,
            KeyCode::KEY_Q,
            KeyCode::KEY_R,
            KeyCode::KEY_S,
            KeyCode::KEY_T,
            KeyCode::KEY_U,
            KeyCode::KEY_V,
            KeyCode::KEY_W,
            KeyCode::KEY_X,
            KeyCode::KEY_Y,
            KeyCode::KEY_Z,
        ];
        const LETTER_NAMES: [&str; 26] = [
            "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q",
            "r", "s", "t", "u", "v", "w", "x", "y", "z",
        ];
        for (name, key) in LETTER_NAMES.iter().zip(LETTERS) {
            map.insert(*name, key);
        }

        map
    })
}

/// Resolve one key token. Accepts friendly aliases (`super`, `pgdn`) and raw
/// evdev names (`KEY_COMMA`, `key_f13`).
pub fn key_from_name(name: &str) -> Option<KeyCode> {
    let cleaned = name.trim().trim_matches(['<', '>']).to_ascii_lowercase();
    if cleaned.is_empty() {
        return None;
    }
    if let Some(key) = aliases().get(cleaned.as_str()) {
        return Some(*key);
    }
    let stripped = cleaned.strip_prefix("key_").unwrap_or(&cleaned);
    aliases().get(stripped).copied()
}

/// Parse a chord such as `SUPER+ALT+D` into its component keys.
///
/// Returns `Err` naming the token that could not be resolved; callers surface
/// that rather than silently binding something unexpected.
pub fn parse_chord(chord: &str) -> Result<Vec<KeyCode>, String> {
    let mut keys = Vec::new();
    for part in chord.split('+') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match key_from_name(part) {
            Some(key) => {
                if !keys.contains(&key) {
                    keys.push(key);
                }
            }
            None => return Err(format!("unknown key '{part}' in shortcut '{chord}'")),
        }
    }
    if keys.is_empty() {
        return Err(format!("shortcut '{chord}' resolved to no keys"));
    }
    Ok(keys)
}

pub fn chord_to_string(keys: &[KeyCode]) -> String {
    keys.iter()
        .map(|key| format!("{key:?}").trim_start_matches("KEY_").to_string())
        .collect::<Vec<_>>()
        .join("+")
}

/// Keystroke for a character on a US QWERTY layout.
///
/// English is the primary target, so ASCII is emitted directly as key events;
/// anything outside this table has to take the clipboard path.
pub fn char_to_key(c: char) -> Option<(KeyCode, bool)> {
    use KeyCode as K;
    let plain = |k: KeyCode| Some((k, false));
    let shifted = |k: KeyCode| Some((k, true));
    match c {
        'a'..='z' => {
            let idx = c as u8 - b'a';
            key_from_name(&((b'a' + idx) as char).to_string()).map(|k| (k, false))
        }
        'A'..='Z' => {
            let idx = c as u8 - b'A';
            key_from_name(&((b'a' + idx) as char).to_string()).map(|k| (k, true))
        }
        '0' => plain(K::KEY_0),
        '1' => plain(K::KEY_1),
        '2' => plain(K::KEY_2),
        '3' => plain(K::KEY_3),
        '4' => plain(K::KEY_4),
        '5' => plain(K::KEY_5),
        '6' => plain(K::KEY_6),
        '7' => plain(K::KEY_7),
        '8' => plain(K::KEY_8),
        '9' => plain(K::KEY_9),
        ' ' => plain(K::KEY_SPACE),
        '\n' => plain(K::KEY_ENTER),
        '\t' => plain(K::KEY_TAB),
        '-' => plain(K::KEY_MINUS),
        '=' => plain(K::KEY_EQUAL),
        '[' => plain(K::KEY_LEFTBRACE),
        ']' => plain(K::KEY_RIGHTBRACE),
        '\\' => plain(K::KEY_BACKSLASH),
        ';' => plain(K::KEY_SEMICOLON),
        '\'' => plain(K::KEY_APOSTROPHE),
        '`' => plain(K::KEY_GRAVE),
        ',' => plain(K::KEY_COMMA),
        '.' => plain(K::KEY_DOT),
        '/' => plain(K::KEY_SLASH),
        '!' => shifted(K::KEY_1),
        '@' => shifted(K::KEY_2),
        '#' => shifted(K::KEY_3),
        '$' => shifted(K::KEY_4),
        '%' => shifted(K::KEY_5),
        '^' => shifted(K::KEY_6),
        '&' => shifted(K::KEY_7),
        '*' => shifted(K::KEY_8),
        '(' => shifted(K::KEY_9),
        ')' => shifted(K::KEY_0),
        '_' => shifted(K::KEY_MINUS),
        '+' => shifted(K::KEY_EQUAL),
        '{' => shifted(K::KEY_LEFTBRACE),
        '}' => shifted(K::KEY_RIGHTBRACE),
        '|' => shifted(K::KEY_BACKSLASH),
        ':' => shifted(K::KEY_SEMICOLON),
        '"' => shifted(K::KEY_APOSTROPHE),
        '~' => shifted(K::KEY_GRAVE),
        '<' => shifted(K::KEY_COMMA),
        '>' => shifted(K::KEY_DOT),
        '?' => shifted(K::KEY_SLASH),
        _ => None,
    }
}

pub fn is_typable(text: &str) -> bool {
    text.chars().all(|c| char_to_key(c).is_some())
}

/// Characters that would have to go through the clipboard, deduplicated.
pub fn untypable_chars(text: &str) -> Vec<char> {
    let mut seen = Vec::new();
    for c in text.chars() {
        if char_to_key(c).is_none() && !seen.contains(&c) {
            seen.push(c);
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_resolve_friendly_and_raw_key_names() {
        let keys = parse_chord("SUPER+ALT+D").unwrap();
        assert_eq!(
            keys,
            vec![KeyCode::KEY_LEFTMETA, KeyCode::KEY_LEFTALT, KeyCode::KEY_D]
        );
        assert_eq!(parse_chord("KEY_F13").unwrap(), vec![KeyCode::KEY_F13]);
        assert_eq!(parse_chord("<f12>").unwrap(), vec![KeyCode::KEY_F12]);
    }

    #[test]
    fn unknown_chord_tokens_are_reported_not_ignored() {
        let err = parse_chord("SUPER+NOPE").unwrap_err();
        assert!(err.contains("NOPE"), "{err}");
    }

    #[test]
    fn duplicate_tokens_collapse() {
        assert_eq!(parse_chord("ctrl+control+c").unwrap().len(), 2);
    }

    #[test]
    fn ascii_maps_to_keystrokes_with_the_right_shift_state() {
        assert_eq!(char_to_key('a'), Some((KeyCode::KEY_A, false)));
        assert_eq!(char_to_key('A'), Some((KeyCode::KEY_A, true)));
        assert_eq!(char_to_key('?'), Some((KeyCode::KEY_SLASH, true)));
        assert_eq!(char_to_key('\n'), Some((KeyCode::KEY_ENTER, false)));
    }

    #[test]
    fn non_ascii_has_no_keystroke_and_is_reported() {
        assert_eq!(char_to_key('é'), None);
        assert!(!is_typable("café"));
        assert!(is_typable("plain ASCII, 100%!"));
        assert_eq!(untypable_chars("héllo wörld é"), vec!['é', 'ö']);
    }

    #[test]
    fn chord_round_trips_through_its_display_form() {
        let keys = parse_chord("ctrl+shift+v").unwrap();
        assert_eq!(chord_to_string(&keys), "LEFTCTRL+LEFTSHIFT+V");
    }
}
