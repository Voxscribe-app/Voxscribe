use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::core::paths;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub general: General,
    pub shortcuts: Shortcuts,
    pub audio: Audio,
    pub asr: Asr,
    pub translation: Translation,
    pub text: Text,
    pub input: Input,
    pub models: Models,
    pub integrations: Integrations,
    pub osd: Osd,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct General {
    pub recording_mode: RecordingMode,
    pub language: Option<String>,
    pub auto_submit: bool,
    pub history_limit: usize,
    pub tap_threshold_ms: u64,
}

impl Default for General {
    fn default() -> Self {
        Self {
            recording_mode: RecordingMode::Toggle,
            language: Some("en".into()),
            auto_submit: false,
            history_limit: 200,
            tap_threshold_ms: 400,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingMode {
    Toggle,
    PushToTalk,
    Auto,
    Continuous,
    LongForm,
}

impl RecordingMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Toggle => "toggle",
            Self::PushToTalk => "push_to_talk",
            Self::Auto => "auto",
            Self::Continuous => "continuous",
            Self::LongForm => "long_form",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "toggle" => Some(Self::Toggle),
            "push_to_talk" | "ptt" | "hold" => Some(Self::PushToTalk),
            "auto" | "hybrid" => Some(Self::Auto),
            "continuous" | "vad" | "automatic" => Some(Self::Continuous),
            "long_form" | "longform" => Some(Self::LongForm),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Shortcuts {
    pub primary: String,
    pub secondary: Option<String>,
    pub secondary_language: Option<String>,
    pub cancel: Option<String>,
    pub long_form_submit: Option<String>,
    pub grab_keys: bool,
    pub device_names: Vec<String>,
    pub device_path: Option<PathBuf>,
    pub hotplug: bool,
    pub debounce_ms: u64,
}

impl Default for Shortcuts {
    fn default() -> Self {
        Self {
            primary: "SUPER+ALT+D".into(),
            secondary: None,
            secondary_language: None,
            cancel: None,
            long_form_submit: None,
            grab_keys: false,
            device_names: Vec::new(),
            device_path: None,
            hotplug: true,
            debounce_ms: 120,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Audio {
    pub device: Option<String>,
    pub device_match: Option<String>,
    pub sample_rate: u32,
    pub keepalive: bool,
    pub feedback: bool,
    pub volume: f32,
    pub start_sound: Option<PathBuf>,
    pub stop_sound: Option<PathBuf>,
    pub error_sound: Option<PathBuf>,
    pub ducking: bool,
    pub ducking_percent: u8,
    pub mute_detection: bool,
    pub silence_timeout: f32,
    pub continuous_silence_seconds: f32,
    pub silence_threshold: f32,
    pub max_recording_seconds: u32,
    pub long_form_segment_seconds: u32,
    pub long_form_limit_mb: u32,
}

impl Default for Audio {
    fn default() -> Self {
        Self {
            device: None,
            device_match: None,
            sample_rate: 16_000,
            keepalive: false,
            feedback: true,
            volume: 0.5,
            start_sound: None,
            stop_sound: None,
            error_sound: None,
            ducking: false,
            ducking_percent: 50,
            mute_detection: true,
            silence_timeout: 0.0,
            continuous_silence_seconds: 2.0,
            silence_threshold: 0.0,
            max_recording_seconds: 900,
            long_form_segment_seconds: 300,
            long_form_limit_mb: 500,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Asr {
    pub backend: String,
    pub fallback: Vec<String>,
    pub whisper: WhisperConfig,
    pub remote: RemoteConfig,
}

impl Default for Asr {
    fn default() -> Self {
        Self {
            backend: "whisper".into(),
            fallback: Vec::new(),
            whisper: WhisperConfig::default(),
            remote: RemoteConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct WhisperConfig {
    pub model: String,
    pub threads: Option<usize>,
    pub prompt: String,
    pub translate: bool,
    pub beam_size: usize,
    pub strategy: SamplingStrategy,
    pub use_gpu: bool,
    pub temperature: f32,
    pub suppress_non_speech: bool,
}

impl Default for WhisperConfig {
    fn default() -> Self {
        Self {
            model: "base.en".into(),
            threads: None,
            prompt: "Transcribe with proper capitalization, including sentence beginnings, \
                     proper nouns, titles, and standard English capitalization rules."
                .into(),
            translate: false,
            beam_size: 5,
            strategy: SamplingStrategy::BeamSearch,
            use_gpu: true,
            temperature: 0.0,
            suppress_non_speech: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SamplingStrategy {
    Greedy,
    BeamSearch,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct RemoteConfig {
    pub url: String,
    pub protocol: RemoteProtocol,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub timeout_ms: u64,
    pub warmup: bool,
    pub headers: BTreeMap<String, String>,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            url: "http://127.0.0.1:8787".into(),
            protocol: RemoteProtocol::Auto,
            model: None,
            api_key: None,
            timeout_ms: 15_000,
            warmup: true,
            headers: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteProtocol {
    Auto,
    Pcm,
    Multipart,
    #[serde(rename = "openai")]
    Openai,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Translation {
    pub target: Option<String>,
    pub source: Option<String>,
    pub skip_when_same: bool,
    pub fallback_to_original: bool,
    pub timeout_ms: u64,
}

impl Default for Translation {
    fn default() -> Self {
        Self {
            target: None,
            source: None,
            skip_when_same: true,
            fallback_to_original: true,
            timeout_ms: 8_000,
        }
    }
}

impl Translation {
    pub fn target_code(&self) -> Option<String> {
        normalize_language(self.target.as_deref())
    }

    pub fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.timeout_ms.max(1))
    }

    fn validate(&self) -> Result<()> {
        for (field, value) in [("target", &self.target), ("source", &self.source)] {
            let Some(raw) = value.as_deref() else {
                continue;
            };
            if raw.trim().is_empty() {
                continue;
            }
            if normalize_language(Some(raw)).is_none() {
                anyhow::bail!(
                    "translation.{field} must be a language code like 'es', 'ja' or 'pt-BR'"
                );
            }
        }
        if self.timeout_ms == 0 {
            anyhow::bail!("translation.timeout_ms must be greater than zero");
        }
        Ok(())
    }
}

pub fn normalize_language(value: Option<&str>) -> Option<String> {
    let raw = value?.trim().replace('_', "-");
    let (language, region) = match raw.split_once('-') {
        Some((language, region)) => (language, Some(region)),
        None => (raw.as_str(), None),
    };
    if !(2..=3).contains(&language.len()) || !language.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let language = language.to_ascii_lowercase();
    match region {
        None => Some(language),
        Some(region) if region.len() == 2 && region.chars().all(|c| c.is_ascii_alphabetic()) => {
            Some(format!("{language}-{}", region.to_ascii_uppercase()))
        }
        Some(_) => None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Text {
    pub word_overrides: BTreeMap<String, String>,
    pub filter_filler_words: bool,
    pub filler_words: Vec<String>,
    pub symbol_replacements: bool,
    pub post_hook: Option<String>,
    pub post_hook_timeout_ms: u64,
    pub trailing_space: bool,
    pub drop_hallucinations: bool,
    pub capitalize_first: bool,
}

impl Default for Text {
    fn default() -> Self {
        Self {
            word_overrides: BTreeMap::new(),
            filter_filler_words: false,
            filler_words: ["uh", "um", "er", "ah", "eh", "hmm", "hm", "mm", "mhm"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            symbol_replacements: true,
            post_hook: None,
            post_hook_timeout_ms: 5_000,
            trailing_space: true,
            drop_hallucinations: true,
            capitalize_first: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Input {
    pub mode: InjectMode,
    pub key_delay_us: u64,
    pub submit_delay_ms: u64,
    pub paste_chord: String,
    pub restore_clipboard: bool,
    pub restore_clipboard_delay_ms: u64,
    pub applications: BTreeMap<String, AppRule>,
}

impl Default for Input {
    fn default() -> Self {
        Self {
            mode: InjectMode::Auto,
            key_delay_us: 1_200,
            submit_delay_ms: 40,
            paste_chord: "ctrl+v".into(),
            restore_clipboard: true,
            restore_clipboard_delay_ms: 1_500,
            applications: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InjectMode {
    Type,
    Clipboard,
    Auto,
    None,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AppRule {
    pub mode: Option<InjectMode>,
    pub auto_submit: Option<bool>,
    pub paste_chord: Option<String>,
    pub key_delay_us: Option<u64>,
    pub disabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Models {
    pub dir: Option<PathBuf>,
    pub download_base_url: String,
}

impl Default for Models {
    fn default() -> Self {
        Self {
            dir: None,
            download_base_url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Integrations {
    pub notifications: bool,
    pub osd: bool,
    pub legacy_state_files: bool,
    pub state_hook: Option<String>,
}

impl Default for Integrations {
    fn default() -> Self {
        Self {
            notifications: true,
            osd: true,
            legacy_state_files: true,
            state_hook: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Osd {
    pub enabled: OsdMode,
    pub position: OsdPosition,
    pub margin: u32,
    pub width: u32,
    pub height: u32,
    pub radius: u32,
    pub bars: usize,
    pub opacity: f32,
    pub background: String,
    pub accent: String,
    pub linger_ms: u64,
}

impl Default for Osd {
    fn default() -> Self {
        Self {
            enabled: OsdMode::Auto,
            position: OsdPosition::Top,
            margin: 48,
            width: 172,
            height: 36,
            radius: 16,
            bars: 16,
            opacity: 0.92,
            background: "#1c1c1e".into(),
            accent: "#ffffff".into(),
            linger_ms: 450,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OsdMode {
    Auto,
    On,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OsdPosition {
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Center,
}

impl Config {
    pub fn load() -> Result<Self> {
        Self::load_from(&paths::config_file())
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&raw).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn parse(raw: &str) -> Result<Self> {
        let config: Self = toml::from_str(raw)?;
        config.validate()?;
        Ok(config)
    }

    pub fn load_or_default() -> Self {
        match Self::load() {
            Ok(config) => config,
            Err(err) => {
                tracing::warn!("using default configuration: {err:#}");
                Self::default()
            }
        }
    }

    pub fn save(&self) -> Result<()> {
        paths::ensure_private_dir(&paths::config_dir())
            .context("securing the configuration directory")?;
        self.save_to(&paths::config_file())
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let body = toml::to_string_pretty(self)?;
        let text = format!("# Duskr configuration - see `duskr config help`.\n{body}");
        paths::write_atomic(path, text.as_bytes())
            .with_context(|| format!("writing {}", path.display()))?;
        let mut permissions = std::fs::metadata(path)?.permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o600);
        std::fs::set_permissions(path, permissions)?;
        Ok(())
    }

    pub fn models_dir(&self) -> PathBuf {
        if let Some(dir) = std::env::var_os("DUSKR_MODEL_DIR") {
            let dir = PathBuf::from(dir);
            if !dir.as_os_str().is_empty() {
                return dir;
            }
        }
        self.models_dir_configured()
    }

    pub fn models_dir_configured(&self) -> PathBuf {
        self.models
            .dir
            .clone()
            .unwrap_or_else(paths::default_models_dir)
    }

    pub fn whisper_threads(&self) -> usize {
        self.asr.whisper.threads.unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get().min(8))
                .unwrap_or(4)
        })
    }

    pub fn app_rule(&self, identifiers: &[String]) -> Option<&AppRule> {
        for ident in identifiers {
            if let Some(rule) = self.input.applications.get(ident) {
                return Some(rule);
            }
        }
        None
    }

    pub fn validate(&self) -> Result<()> {
        if !(8_000..=192_000).contains(&self.audio.sample_rate) {
            anyhow::bail!("audio.sample_rate must be between 8000 and 192000");
        }
        if !self.audio.volume.is_finite() || !(0.0..=1.0).contains(&self.audio.volume) {
            anyhow::bail!("audio.volume must be between 0 and 1");
        }
        if !self.audio.silence_timeout.is_finite() || self.audio.silence_timeout < 0.0 {
            anyhow::bail!("audio.silence_timeout must be non-negative");
        }
        if self.asr.whisper.model.trim().is_empty() {
            anyhow::bail!("asr.whisper.model must not be empty");
        }
        let backend = self.asr.backend.trim().to_ascii_lowercase();
        if matches!(
            backend.as_str(),
            "remote" | "parakeet" | "rest-api" | "rest" | "http"
        ) && !(self.asr.remote.url.starts_with("http://")
            || self.asr.remote.url.starts_with("https://"))
        {
            anyhow::bail!("asr.remote.url must use http or https");
        }
        self.translation.validate()?;
        self.osd.validate()?;
        Ok(())
    }
}

impl Osd {
    fn validate(&self) -> Result<()> {
        if !self.opacity.is_finite() || !(0.0..=1.0).contains(&self.opacity) {
            anyhow::bail!("osd.opacity must be between 0 and 1");
        }
        if !(1..=64).contains(&self.bars) {
            anyhow::bail!("osd.bars must be between 1 and 64");
        }
        if !(48..=1024).contains(&self.width) || !(16..=256).contains(&self.height) {
            anyhow::bail!("osd.width must be 48-1024 and osd.height 16-256");
        }
        for (field, value) in [("background", &self.background), ("accent", &self.accent)] {
            if crate::integrations::osd::render::Color::parse(value).is_none() {
                anyhow::bail!("osd.{field} must be a hex colour like #7aa2f7");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_through_toml() {
        let config = Config::default();
        let text = toml::to_string_pretty(&config).unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn partial_config_keeps_defaults_for_absent_sections() {
        let config = Config::parse("[general]\nrecording_mode = \"push_to_talk\"\n").unwrap();
        assert_eq!(config.general.recording_mode, RecordingMode::PushToTalk);
        assert_eq!(config.shortcuts.primary, Shortcuts::default().primary);
        assert_eq!(config.asr.backend, "whisper");
    }

    #[test]
    fn unknown_keys_are_reported_rather_than_silently_dropped() {
        let err = Config::parse("[general]\nnope = 1\n").unwrap_err();
        assert!(err.to_string().contains("nope"), "{err}");
    }

    #[test]
    fn recording_mode_parses_hyprwhspr_spellings() {
        assert_eq!(
            RecordingMode::parse("push_to_talk"),
            Some(RecordingMode::PushToTalk)
        );
        assert_eq!(
            RecordingMode::parse("LONG-FORM"),
            Some(RecordingMode::LongForm)
        );
        assert_eq!(RecordingMode::parse("nonsense"), None);
    }

    #[test]
    fn invalid_runtime_values_are_rejected() {
        assert!(Config::parse("[audio]\nvolume = 2.0\n").is_err());
        assert!(Config::parse(
            "[asr]\nbackend = \"remote\"\n[asr.remote]\nurl = \"file:///tmp\"\n"
        )
        .is_err());
    }

    #[test]
    fn env_override_wins_over_configured_model_dir() {
        let mut config = Config::default();
        config.models.dir = Some(PathBuf::from("/configured"));
        assert_eq!(config.models_dir(), PathBuf::from("/configured"));
        std::env::set_var("DUSKR_MODEL_DIR", "/override");
        assert_eq!(config.models_dir(), PathBuf::from("/override"));
        assert_eq!(config.models_dir_configured(), PathBuf::from("/configured"));
        std::env::remove_var("DUSKR_MODEL_DIR");
    }
}
