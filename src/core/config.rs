//! Duskr's on-disk configuration.
//!
//! Every field carries a default so a config file only names what it changes.

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
    pub text: Text,
    pub input: Input,
    pub models: Models,
    pub integrations: Integrations,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct General {
    pub recording_mode: RecordingMode,
    /// Transcription language; `None` lets the backend auto-detect.
    pub language: Option<String>,
    /// Send Enter after the transcript lands.
    pub auto_submit: bool,
    /// Keep the last N transcripts in `history.jsonl`; 0 disables history.
    pub history_limit: usize,
    /// Tap-vs-hold boundary for [`RecordingMode::Auto`].
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
    /// Press to start, press again to stop.
    Toggle,
    /// Record while the shortcut is held.
    PushToTalk,
    /// Tap toggles, hold behaves as push-to-talk.
    Auto,
    /// Stays open and flushes a transcript on every speech pause.
    Continuous,
    /// Segmented recording with pause/resume, submitted explicitly.
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
    /// Language forced by the secondary shortcut.
    pub secondary_language: Option<String>,
    pub cancel: Option<String>,
    pub long_form_submit: Option<String>,
    /// Grab keyboards exclusively so the chord never reaches other apps.
    pub grab_keys: bool,
    /// Allowlist of keyboard device names; empty means "every keyboard".
    pub device_names: Vec<String>,
    pub device_path: Option<PathBuf>,
    /// Pick up keyboards plugged in after startup.
    pub hotplug: bool,
    /// Ignore repeated triggers within this window.
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
    /// PipeWire target: node name, node id, or `None` for the default source.
    pub device: Option<String>,
    /// PipeWire node name or id used when `device` is unset.
    pub device_match: Option<String>,
    pub sample_rate: u32,
    /// Hold the capture stream open between recordings (mic stays warm, but the
    /// desktop shows a permanent microphone-in-use indicator).
    pub keepalive: bool,
    pub feedback: bool,
    pub volume: f32,
    pub start_sound: Option<PathBuf>,
    pub stop_sound: Option<PathBuf>,
    pub error_sound: Option<PathBuf>,
    pub ducking: bool,
    pub ducking_percent: u8,
    /// Abort a recording that only ever sees digital silence.
    pub mute_detection: bool,
    /// Auto-stop after this many seconds of silence (0 = never). Arms only once
    /// speech has been heard, so a slow start is not cut off.
    pub silence_timeout: f32,
    /// Continuous mode: silence needed before a segment is flushed.
    pub continuous_silence_seconds: f32,
    /// RMS below which audio counts as silence; 0 auto-calibrates per session.
    pub silence_threshold: f32,
    /// Hard cap on a single recording.
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
    /// Active backend id; see `duskr backend list`.
    pub backend: String,
    /// Backends tried in order when `backend` fails to load or transcribe.
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
    /// Model name (`medium.en`) or an absolute path to a ggml file.
    pub model: String,
    pub threads: Option<usize>,
    pub prompt: String,
    /// Emit English regardless of the spoken language.
    pub translate: bool,
    pub beam_size: usize,
    pub strategy: SamplingStrategy,
    pub use_gpu: bool,
    pub temperature: f32,
    /// Whisper's own VAD-ish suppression of non-speech tokens.
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
    /// Wire format: `pcm` posts raw 16 kHz mono s16le, `multipart` uploads a
    /// WAV part, `auto` tries PCM once and remembers the answer.
    pub protocol: RemoteProtocol,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub timeout_ms: u64,
    /// Open the connection at daemon start so the first dictation is not paying
    /// for a TCP + TLS handshake.
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Text {
    /// Case-insensitive whole-word replacements applied to every transcript.
    pub word_overrides: BTreeMap<String, String>,
    pub filter_filler_words: bool,
    pub filler_words: Vec<String>,
    /// Turn spoken punctuation ("comma", "new line") into characters.
    pub symbol_replacements: bool,
    /// Shell command receiving the transcript on stdin; non-empty stdout wins.
    pub post_hook: Option<String>,
    pub post_hook_timeout_ms: u64,
    /// Append a space so consecutive dictations do not run together.
    pub trailing_space: bool,
    /// Drop transcripts that are only a known Whisper silence hallucination.
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
    /// Gap between synthetic key events. Too small and fast consumers (games,
    /// Electron) drop characters; too large and long transcripts crawl.
    pub key_delay_us: u64,
    /// Extra settle time after the last keystroke before Enter is sent.
    pub submit_delay_ms: u64,
    /// Chord used when a transcript has to go through the clipboard.
    pub paste_chord: String,
    /// Restore the pre-existing clipboard after a clipboard paste.
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
    /// Synthesize keystrokes on the virtual keyboard. Clipboard is never touched.
    Type,
    /// Always paste; the previous clipboard is saved and restored.
    Clipboard,
    /// Type, falling back to the clipboard only for unrepresentable characters.
    Auto,
    /// Transcribe but inject nothing (IPC subscribers still see the text).
    None,
}

/// Per-application override, keyed by a normalized window class/app-id.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AppRule {
    pub mode: Option<InjectMode>,
    pub auto_submit: Option<bool>,
    pub paste_chord: Option<String>,
    pub key_delay_us: Option<u64>,
    /// Inject nothing at all while this app is focused.
    pub disabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Models {
    /// Overridden at runtime by `DUSKR_MODEL_DIR`.
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
    /// Emit an OSD hint over D-Bus while recording.
    pub osd: bool,
    /// Mirror state/level into the config dir for Waybar-style pollers.
    pub legacy_state_files: bool,
    /// Command run on every state change, receiving the state JSON on stdin.
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

    /// Read the config, falling back to defaults (with a warning) when the file
    /// is unreadable - a typo in the config must not make the daemon unstartable.
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

    /// Directory holding local ASR models. `DUSKR_MODEL_DIR` wins so a shell can
    /// point one invocation somewhere else without editing the config.
    pub fn models_dir(&self) -> PathBuf {
        if let Some(dir) = std::env::var_os("DUSKR_MODEL_DIR") {
            let dir = PathBuf::from(dir);
            if !dir.as_os_str().is_empty() {
                return dir;
            }
        }
        self.models_dir_configured()
    }

    /// Configured model directory, ignoring the environment override.
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

    /// Rule for a focused window, matched against normalized identifiers.
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
