
use std::time::Duration;

use anyhow::{Context, Result};
use translators::{GoogleTranslator, Translator};

use crate::core::config::{normalize_language, Config};

pub fn source_code(config: &Config) -> String {
    if let Some(explicit) = normalize_language(config.translation.source.as_deref()) {
        return explicit;
    }
    if config.asr.whisper.translate
        && crate::asr::canonical_backend_id(&config.asr.backend) == "whisper"
    {
        return "en".into();
    }
    normalize_language(config.general.language.as_deref()).unwrap_or_default()
}

fn wanted(config: &Config) -> Option<(String, String)> {
    let target = config.translation.target_code()?;
    let source = source_code(config);
    if config.translation.skip_when_same && !source.is_empty() && source == target {
        return None;
    }
    Some((source, target))
}

pub struct Outcome {
    pub text: String,
    pub source: Option<String>,
    pub target: Option<String>,
}

impl Outcome {
    pub fn empty() -> Self {
        Self::untouched("")
    }

    fn untouched(text: &str) -> Self {
        Self {
            text: text.to_string(),
            source: None,
            target: None,
        }
    }
}

pub async fn apply(text: &str, config: &Config) -> Outcome {
    if text.trim().is_empty() {
        return Outcome::untouched(text);
    }
    let Some((source, target)) = wanted(config) else {
        return Outcome::untouched(text);
    };

    let text = match translate(text, &source, &target, config.translation.timeout()).await {
        Ok(translated) => translated,
        Err(err) => {
            tracing::warn!("translation to '{target}' failed: {err:#}");
            if config.translation.fallback_to_original {
                text.to_string()
            } else {
                String::new()
            }
        }
    };
    Outcome {
        text,
        source: Some(source),
        target: Some(target),
    }
}

pub async fn translate(
    text: &str,
    source: &str,
    target: &str,
    timeout: Duration,
) -> Result<String> {
    let translator = GoogleTranslator {
        timeout: timeout.as_secs().max(1) as usize,
        ..GoogleTranslator::default()
    };

    let translated = translator
        .translate_async(text, source, target)
        .await
        .with_context(|| {
            let from = if source.is_empty() { "auto" } else { source };
            format!("translating from {from} to {target}")
        })?;

    if translated.trim().is_empty() {
        anyhow::bail!("the translator returned an empty result");
    }
    Ok(translated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(target: Option<&str>, source: Option<&str>) -> Config {
        let mut config = Config::default();
        config.translation.target = target.map(str::to_string);
        config.translation.source = source.map(str::to_string);
        config
    }

    #[tokio::test]
    async fn an_unset_target_leaves_the_transcript_alone() {
        let config = config(None, None);
        let outcome = apply("hello world", &config).await;
        assert_eq!(outcome.text, "hello world");
        assert_eq!(outcome.target, None);
    }

    #[test]
    fn the_source_falls_back_to_the_dictation_language() {
        let mut config = config(Some("ja"), None);
        config.general.language = Some("EN_us".into());
        assert_eq!(source_code(&config), "en-US");
    }

    #[test]
    fn an_explicit_source_wins_over_the_dictation_language() {
        let mut config = config(Some("ja"), Some("de"));
        config.general.language = Some("en".into());
        assert_eq!(source_code(&config), "de");
    }

    #[test]
    fn whisper_translating_to_english_first_is_taken_into_account() {
        let mut config = config(Some("ja"), None);
        config.general.language = Some("de".into());
        config.asr.whisper.translate = true;
        assert_eq!(source_code(&config), "en");
    }

    #[test]
    fn auto_detection_is_requested_when_no_language_is_configured() {
        let mut config = config(Some("ja"), None);
        config.general.language = None;
        assert_eq!(source_code(&config), "");
    }

    #[test]
    fn translating_a_language_into_itself_is_skipped() {
        let config = config(Some("fr"), Some("fr"));
        assert_eq!(wanted(&config), None);
    }

    #[test]
    fn the_skip_can_be_disabled_for_round_tripping() {
        let mut config = config(Some("fr"), Some("fr"));
        config.translation.skip_when_same = false;
        assert_eq!(wanted(&config), Some(("fr".into(), "fr".into())));
    }

    #[tokio::test]
    #[ignore]
    async fn a_real_round_trip_reaches_google() {
        let translated = translate(
            "Hello world, how are you?",
            "en",
            "fr",
            Duration::from_secs(15),
        )
        .await
        .expect("translation request");
        assert!(
            translated.to_lowercase().contains("bonjour"),
            "{translated}"
        );
    }

    #[test]
    fn a_detected_source_never_counts_as_matching_the_target() {
        let mut config = config(Some("fr"), None);
        config.general.language = None;
        assert_eq!(wanted(&config), Some((String::new(), "fr".into())));
    }
}
