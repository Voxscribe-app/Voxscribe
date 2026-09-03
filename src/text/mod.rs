//! Transcript post-processing: the transform between raw ASR output and the
//! characters actually typed into the focused window.
//!
//! Order matters and mirrors hyprwhspr: newline flattening, user word
//! overrides, filler removal, spoken-symbol substitution, whitespace collapse,
//! then the optional user hook.

pub mod hook;

use std::sync::OnceLock;

use regex::{Regex, RegexBuilder};

use crate::core::config::Text;

/// Phrases Whisper emits for silence or background noise. A transcript made up
/// entirely of one of these is discarded rather than typed.
const HALLUCINATIONS: &[&str] = &[
    "blank audio",
    "blank",
    "silence",
    "no speech",
    "you",
    "thank you",
    "thanks for watching",
    "thank you for watching",
    "video playback",
    "music",
    "music playing",
    "keyboard clicking",
    "[blank_audio]",
    "(upbeat music)",
];

/// Spoken punctuation, longest phrase first so "question mark" wins over "mark".
const SYMBOLS: &[(&str, &str)] = &[
    ("question mark", "?"),
    ("exclamation mark", "!"),
    ("exclamation point", "!"),
    ("open paren", "("),
    ("close paren", ")"),
    ("open bracket", "["),
    ("close bracket", "]"),
    ("open brace", "{"),
    ("close brace", "}"),
    ("at symbol", "@"),
    ("dollar sign", "$"),
    ("less than", "<"),
    ("greater than", ">"),
    ("new line", "\n"),
    ("newline", "\n"),
    ("semicolon", ";"),
    ("apostrophe", "'"),
    ("underscore", "_"),
    ("ampersand", "&"),
    ("asterisk", "*"),
    ("backslash", "\\"),
    ("percent", "%"),
    ("period", "."),
    ("comma", ","),
    ("colon", ":"),
    ("dash", "-"),
    ("hash", "#"),
    ("caret", "^"),
    ("plus", "+"),
    ("equals", "="),
    ("slash", "/"),
    ("pipe", "|"),
    ("tilde", "~"),
    ("grave", "`"),
    ("quote", "\""),
    ("tab", "\t"),
];

fn symbol_regexes() -> &'static Vec<(Regex, &'static str)> {
    static CACHE: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    CACHE.get_or_init(|| {
        SYMBOLS
            .iter()
            .map(|(phrase, replacement)| {
                let pattern = format!(r"\b{}\b", regex::escape(phrase));
                let regex = RegexBuilder::new(&pattern)
                    .case_insensitive(true)
                    .build()
                    .expect("static symbol pattern");
                (regex, *replacement)
            })
            .collect()
    })
}

fn word_regex(word: &str) -> Option<Regex> {
    // Single characters cannot use \b (it would refuse to match mid-word), so
    // they are replaced literally.
    let pattern = if word.chars().count() == 1 {
        regex::escape(word)
    } else {
        format!(r"\b{}\b", regex::escape(word))
    };
    RegexBuilder::new(&pattern)
        .case_insensitive(true)
        .build()
        .ok()
}

/// Apply every configured transform except the external hook, which is async.
pub fn process(raw: &str, config: &Text) -> String {
    let mut text = raw.replace("\r\n", " ").replace(['\r', '\n'], " ");

    text = apply_word_overrides(&text, config);
    text = filter_filler_words(&text, config);

    if config.symbol_replacements {
        for (regex, replacement) in symbol_regexes() {
            text = regex.replace_all(&text, *replacement).into_owned();
        }
        text = tidy_punctuation(&text);
    }

    text = collapse_whitespace(&text);

    if config.capitalize_first {
        text = capitalize_first(&text);
    }

    if config.drop_hallucinations && is_hallucination(&text) {
        return String::new();
    }

    text
}

fn apply_word_overrides(text: &str, config: &Text) -> String {
    if config.word_overrides.is_empty() {
        return text.to_string();
    }
    let mut out = text.to_string();
    for (from, to) in &config.word_overrides {
        if from.is_empty() {
            continue;
        }
        if let Some(regex) = word_regex(from) {
            out = regex.replace_all(&out, to.as_str()).into_owned();
        }
    }
    out
}

fn filter_filler_words(text: &str, config: &Text) -> String {
    if !config.filter_filler_words || config.filler_words.is_empty() {
        return text.to_string();
    }
    let mut out = text.to_string();
    for word in &config.filler_words {
        if word.is_empty() {
            continue;
        }
        if let Some(regex) = word_regex(word) {
            out = regex.replace_all(&out, "").into_owned();
        }
    }
    // Filler removal leaves orphaned punctuation spacing behind.
    let out = collapse_whitespace(&out);
    static ORPHANS: OnceLock<Regex> = OnceLock::new();
    let orphans = ORPHANS.get_or_init(|| Regex::new(r" +([,.!?;:])").expect("static pattern"));
    orphans.replace_all(&out, "$1").into_owned()
}

/// Close the gap a substituted symbol leaves behind: "this ?" -> "this?".
fn tidy_punctuation(text: &str) -> String {
    static BEFORE: OnceLock<Regex> = OnceLock::new();
    static AFTER: OnceLock<Regex> = OnceLock::new();
    let before =
        BEFORE.get_or_init(|| Regex::new(r#" +([,.!?;:%\)\]}])"#).expect("static pattern"));
    let after = AFTER.get_or_init(|| Regex::new(r"([(\[{]) +").expect("static pattern"));
    let out = before.replace_all(text, "$1");
    after.replace_all(&out, "$1").into_owned()
}

fn collapse_whitespace(text: &str) -> String {
    static SPACES: OnceLock<Regex> = OnceLock::new();
    static AROUND_NEWLINE: OnceLock<Regex> = OnceLock::new();
    let spaces = SPACES.get_or_init(|| Regex::new(r"[ \t]+").expect("static pattern"));
    let around_newline =
        AROUND_NEWLINE.get_or_init(|| Regex::new(r" *\n *").expect("static pattern"));
    let out = spaces.replace_all(text, " ");
    let out = around_newline.replace_all(&out, "\n");
    out.trim().to_string()
}

fn capitalize_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn is_hallucination(text: &str) -> bool {
    let lowered = text.trim().to_lowercase();
    if lowered.is_empty() {
        return true;
    }
    // Markers are matched both verbatim (they may carry their own brackets) and
    // stripped of trailing punctuation ("Thank you." -> "thank you").
    let stripped = lowered
        .trim_matches(|c: char| c.is_ascii_punctuation())
        .trim();
    HALLUCINATIONS
        .iter()
        .any(|marker| lowered == *marker || stripped == *marker)
}

/// Text as it should reach the keyboard: trailing newlines stripped (they would
/// submit forms early) plus the optional separator space.
pub fn finalize_for_injection(text: &str, config: &Text) -> String {
    let trimmed = text.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() {
        return String::new();
    }
    if config.trailing_space {
        format!("{trimmed} ")
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Text {
        Text::default()
    }

    #[test]
    fn newlines_become_spaces_so_dictation_never_submits_early() {
        let out = process("hello\nworld\r\nagain", &config());
        assert_eq!(out, "hello world again");
    }

    #[test]
    fn spoken_symbols_are_substituted_longest_phrase_first() {
        let out = process("what is this question mark", &config());
        assert_eq!(out, "what is this?");
    }

    #[test]
    fn symbol_substitution_can_be_disabled() {
        let mut config = config();
        config.symbol_replacements = false;
        assert_eq!(process("wait comma please", &config), "wait comma please");
    }

    #[test]
    fn word_overrides_are_case_insensitive_and_word_bounded() {
        let mut config = config();
        config
            .word_overrides
            .insert("hyper whisper".into(), "hyprwhspr".into());
        assert_eq!(process("Hyper Whisper rocks", &config), "hyprwhspr rocks");
    }

    #[test]
    fn word_overrides_do_not_match_inside_longer_words() {
        let mut config = config();
        config.word_overrides.insert("cat".into(), "dog".into());
        assert_eq!(
            process("concatenate the cat", &config),
            "concatenate the dog"
        );
    }

    #[test]
    fn filler_removal_tidies_the_punctuation_it_orphans() {
        let mut config = config();
        config.filter_filler_words = true;
        assert_eq!(process("so um , yes", &config), "so, yes");
    }

    #[test]
    fn filler_words_are_kept_unless_filtering_is_enabled() {
        assert_eq!(process("so um yes", &config()), "so um yes");
    }

    #[test]
    fn silence_hallucinations_are_dropped_entirely() {
        assert_eq!(process("Thank you.", &config()), "");
        assert_eq!(process("[BLANK_AUDIO]", &config()), "");
        assert_eq!(
            process("thank you for the coffee", &config()),
            "thank you for the coffee"
        );
    }

    #[test]
    fn hallucination_dropping_can_be_disabled() {
        let mut config = config();
        config.drop_hallucinations = false;
        assert_eq!(process("Thank you.", &config), "Thank you.");
    }

    #[test]
    fn finalize_appends_a_separator_space_and_strips_newlines() {
        let config = config();
        assert_eq!(finalize_for_injection("hi\n", &config), "hi ");
        let mut no_space = config.clone();
        no_space.trailing_space = false;
        assert_eq!(finalize_for_injection("hi", &no_space), "hi");
        assert_eq!(finalize_for_injection("\n", &config), "");
    }

    #[test]
    fn new_line_keyword_survives_whitespace_collapsing() {
        assert_eq!(process("first new line second", &config()), "first\nsecond");
    }
}
