pub mod quickshell;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::asr;
use crate::core::config::{Config, RecordingMode, RemoteProtocol, SamplingStrategy};
use crate::core::paths;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    Mapped { from: String, to: String },
    Dropped { key: String, reason: String },
    Warning(String),
}

impl std::fmt::Display for Note {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Note::Mapped { from, to } => write!(f, "  {from} -> {to}"),
            Note::Dropped { key, reason } => write!(f, "  {key}: dropped ({reason})"),
            Note::Warning(message) => write!(f, "  ! {message}"),
        }
    }
}

fn as_str(value: Option<&Value>) -> Option<String> {
    value?
        .as_str()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

fn as_bool(value: Option<&Value>) -> Option<bool> {
    value?.as_bool()
}

fn as_f32(value: Option<&Value>) -> Option<f32> {
    value?.as_f64().map(|v| v as f32)
}

fn as_u64(value: Option<&Value>) -> Option<u64> {
    value?.as_u64()
}

pub fn translate(source: &Value, base: Config) -> (Config, Vec<Note>) {
    let mut config = base;
    let mut notes = Vec::new();
    let get = |key: &str| source.get(key);

    let mode_source = as_str(get("recording_mode")).or_else(|| {
        as_bool(get("push_to_talk")).map(|ptt| {
            if ptt {
                "push_to_talk".to_string()
            } else {
                "toggle".to_string()
            }
        })
    });
    if let Some(raw) = mode_source {
        match RecordingMode::parse(&raw) {
            Some(mode) => {
                config.general.recording_mode = mode;
                notes.push(Note::Mapped {
                    from: format!("recording_mode = {raw}"),
                    to: format!("general.recording_mode = {}", mode.as_str()),
                });
            }
            None => notes.push(Note::Warning(format!(
                "unknown recording_mode '{raw}'; keeping {}",
                config.general.recording_mode.as_str()
            ))),
        }
    }

    if let Some(language) = as_str(get("language")) {
        config.general.language = Some(language.clone());
        notes.push(Note::Mapped {
            from: format!("language = {language}"),
            to: "general.language".into(),
        });
    }
    if let Some(auto_submit) = as_bool(get("auto_submit")) {
        config.general.auto_submit = auto_submit;
        notes.push(Note::Mapped {
            from: format!("auto_submit = {auto_submit}"),
            to: "general.auto_submit".into(),
        });
    }

    if let Some(primary) = as_str(get("primary_shortcut")) {
        config.shortcuts.primary = primary.clone();
        notes.push(Note::Mapped {
            from: format!("primary_shortcut = {primary}"),
            to: "shortcuts.primary".into(),
        });
    }
    config.shortcuts.secondary = as_str(get("secondary_shortcut"));
    config.shortcuts.secondary_language = as_str(get("secondary_language"));
    config.shortcuts.cancel = as_str(get("cancel_shortcut"));
    config.shortcuts.long_form_submit = as_str(get("long_form_submit_shortcut"));
    if let Some(grab) = as_bool(get("grab_keys")) {
        config.shortcuts.grab_keys = grab;
    }
    if let Some(hotplug) = as_bool(get("keyboard_hotplug")) {
        config.shortcuts.hotplug = hotplug;
    }
    if let Some(names) = get("keyboard_device_names").and_then(|v| v.as_array()) {
        config.shortcuts.device_names = names
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        if !config.shortcuts.device_names.is_empty() {
            notes.push(Note::Mapped {
                from: "keyboard_device_names".into(),
                to: "shortcuts.device_names".into(),
            });
        }
    }
    if let Some(path) = as_str(get("selected_device_path")) {
        config.shortcuts.device_path = Some(PathBuf::from(path));
    }
    if as_bool(get("use_hypr_bindings")) == Some(true) {
        notes.push(Note::Warning(
            "use_hypr_bindings was set: Duskr reads evdev directly on every \
             compositor. Its own shortcuts are active - remove the Hyprland \
             bindings, or clear shortcuts.primary to keep using them."
                .into(),
        ));
    }

    if let Some(device) =
        as_str(get("audio_device_name")).or_else(|| as_str(get("audio_device_id")))
    {
        config.audio.device_match = Some(device.clone());
        notes.push(Note::Mapped {
            from: format!("audio_device = {device}"),
            to: "audio.device_match".into(),
        });
    }
    if let Some(feedback) = as_bool(get("audio_feedback")) {
        config.audio.feedback = feedback;
    }
    if let Some(volume) = as_f32(get("audio_volume")) {
        config.audio.volume = volume.clamp(0.0, 1.0);
    }
    for (key, target) in [
        ("start_sound_path", 0usize),
        ("stop_sound_path", 1),
        ("error_sound_path", 2),
    ] {
        if let Some(path) = as_str(get(key)) {
            let path = PathBuf::from(path);
            match target {
                0 => config.audio.start_sound = Some(path),
                1 => config.audio.stop_sound = Some(path),
                _ => config.audio.error_sound = Some(path),
            }
        }
    }
    if let Some(ducking) = as_bool(get("audio_ducking")) {
        config.audio.ducking = ducking;
    }
    if let Some(percent) = as_u64(get("audio_ducking_percent")) {
        config.audio.ducking_percent = percent.min(100) as u8;
    }
    if let Some(mute) = as_bool(get("mute_detection")) {
        config.audio.mute_detection = mute;
    }
    if let Some(keepalive) = as_bool(get("keepalive_stream")) {
        config.audio.keepalive = keepalive;
    }
    if let Some(timeout) = as_f32(get("silence_timeout")) {
        config.audio.silence_timeout = timeout;
    }
    if let Some(seconds) = as_f32(get("continuous_silence_seconds")) {
        config.audio.continuous_silence_seconds = seconds;
    }
    if let Some(threshold) = as_f32(get("continuous_silence_threshold")) {
        config.audio.silence_threshold = threshold;
    }
    if let Some(limit) = as_u64(get("long_form_temp_limit_mb")) {
        config.audio.long_form_limit_mb = limit as u32;
    }
    if let Some(interval) = as_u64(get("long_form_auto_save_interval")) {
        config.audio.long_form_segment_seconds = interval as u32;
    }

    let backend_raw = as_str(get("transcription_backend")).unwrap_or_else(|| "pywhispercpp".into());
    let backend = asr::canonical_backend_id(&backend_raw);
    match backend {
        "whisper" => {
            config.asr.backend = "whisper".into();
            let model = if backend_raw.eq_ignore_ascii_case("faster-whisper") {
                as_str(get("faster_whisper_model")).or_else(|| as_str(get("model")))
            } else {
                as_str(get("model"))
            };
            if let Some(model) = model {
                config.asr.whisper.model = model.clone();
                notes.push(Note::Mapped {
                    from: format!("model = {model}"),
                    to: "asr.whisper.model".into(),
                });
            }
        }
        "remote" => {
            config.asr.backend = "remote".into();
            if let Some(url) = as_str(get("rest_endpoint_url")) {
                config.asr.remote.url = url
                    .trim_end_matches("/transcribe")
                    .trim_end_matches('/')
                    .to_string();
                notes.push(Note::Mapped {
                    from: format!("rest_endpoint_url = {url}"),
                    to: "asr.remote.url".into(),
                });
            }
            config.asr.remote.protocol = RemoteProtocol::Auto;
            if let Some(timeout) = as_u64(get("rest_timeout")) {
                config.asr.remote.timeout_ms = timeout * 1000;
            }
            if let Some(headers) = get("rest_headers").and_then(|v| v.as_object()) {
                config.asr.remote.headers = headers
                    .iter()
                    .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                    .collect();
            }
            if as_str(get("rest_api_key")).is_some() {
                notes.push(Note::Warning(
                    "rest_api_key was set: re-enter it as asr.remote.api_key. \
                     It was not copied, so a shared config never carries a secret \
                     it did not have before."
                        .into(),
                ));
            }
        }
        _ => {
            notes.push(Note::Warning(format!(
                "backend '{backend_raw}' has no Duskr equivalent; keeping '{}'",
                config.asr.backend
            )));
        }
    }

    if let Some(prompt) = as_str(get("whisper_prompt")) {
        config.asr.whisper.prompt = prompt;
        notes.push(Note::Mapped {
            from: "whisper_prompt".into(),
            to: "asr.whisper.prompt".into(),
        });
    }
    if let Some(threads) = as_u64(get("threads")) {
        config.asr.whisper.threads = Some(threads as usize);
    }
    if as_str(get("task")).as_deref() == Some("translate") {
        config.asr.whisper.translate = true;
    }
    if let Some(strategy) = as_str(get("sampling_strategy")) {
        config.asr.whisper.strategy = if strategy == "greedy" {
            SamplingStrategy::Greedy
        } else {
            SamplingStrategy::BeamSearch
        };
    }
    if let Some(beam) = as_u64(get("beam_size")) {
        config.asr.whisper.beam_size = beam.max(1) as usize;
    }

    if let Some(overrides) = get("word_overrides").and_then(|v| v.as_object()) {
        let mapped: BTreeMap<String, String> = overrides
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
            .collect();
        if !mapped.is_empty() {
            notes.push(Note::Mapped {
                from: format!("word_overrides ({} entries)", mapped.len()),
                to: "text.word_overrides".into(),
            });
        }
        config.text.word_overrides = mapped;
    }
    if let Some(filter) = as_bool(get("filter_filler_words")) {
        config.text.filter_filler_words = filter;
    }
    if let Some(words) = get("filler_words").and_then(|v| v.as_array()) {
        let words: Vec<String> = words
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        if !words.is_empty() {
            config.text.filler_words = words;
        }
    }
    if let Some(symbols) = as_bool(get("symbol_replacements")) {
        config.text.symbol_replacements = symbols;
    }
    if let Some(hook) = as_str(get("post_transcription_hook")) {
        config.text.post_hook = Some(hook);
        notes.push(Note::Mapped {
            from: "post_transcription_hook".into(),
            to: "text.post_hook".into(),
        });
    }

    if let Some(mode) = as_str(get("inject_mode")) {
        notes.push(Note::Dropped {
            key: format!("inject_mode = {mode}"),
            reason: "Duskr types on a virtual keyboard instead of using wtype/ydotool".into(),
        });
    }
    for key in [
        "paste_mode",
        "paste_keycode",
        "paste_keycode_wev",
        "shift_paste",
    ] {
        if get(key).is_some_and(|v| !v.is_null()) {
            notes.push(Note::Dropped {
                key: key.into(),
                reason: "only applies to clipboard pasting, which is now a fallback".into(),
            });
        }
    }
    if let Some(applications) = get("applications").and_then(|v| v.as_object()) {
        for (name, rule) in applications {
            let ident = crate::input::window::normalize(name);
            if ident.is_empty() {
                continue;
            }
            let mut app_rule = crate::core::config::AppRule::default();
            match rule.get("auto_paste") {
                Some(Value::Bool(false)) => app_rule.disabled = true,
                Some(Value::String(chord)) => {
                    app_rule.mode = Some(crate::core::config::InjectMode::Clipboard);
                    app_rule.paste_chord = Some(chord.clone());
                }
                _ => {}
            }
            config.input.applications.insert(ident.clone(), app_rule);
            notes.push(Note::Mapped {
                from: format!("applications.{name}"),
                to: format!("input.applications.{ident}"),
            });
        }
    }

    if let Some(osd) = as_bool(get("mic_osd_enabled")) {
        config.integrations.osd = osd;
    }

    (config, notes)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelImport {
    pub name: String,
    pub source: PathBuf,
    pub destination: PathBuf,
}

pub fn model_search_paths() -> Vec<PathBuf> {
    vec![
        paths::pywhispercpp_models_dir(),
        paths::hyprwhspr_data_dir().join("models"),
        paths::hyprwhspr_data_dir().join("whisper.cpp/models"),
    ]
}

pub fn import_models(models_dir: &Path, move_files: bool) -> Result<Vec<ModelImport>> {
    let mut imported = Vec::new();
    for directory in model_search_paths() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let source = entry.path();
            if !source.is_file() {
                continue;
            }
            let name = source
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if !name.starts_with("ggml-") || !name.ends_with(".bin") {
                continue;
            }
            let destination = models_dir.join(name);
            if destination.exists() {
                continue;
            }
            match crate::models::add(models_dir, &source, move_files) {
                Ok(entry) => imported.push(ModelImport {
                    name: entry.name,
                    source,
                    destination: entry.path,
                }),
                Err(err) => tracing::warn!("skipping {}: {err:#}", source.display()),
            }
        }
    }
    Ok(imported)
}

pub fn backup(path: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!("{e}."))
        .unwrap_or_default();
    let backup = path.with_extension(format!("{extension}bak-{stamp}"));
    std::fs::copy(path, &backup).with_context(|| format!("backing up {}", path.display()))?;
    Ok(Some(backup))
}

pub fn hyprwhspr_config_path() -> PathBuf {
    paths::hyprwhspr_config_dir().join("config.json")
}

pub fn read_hyprwhspr_config(path: &Path) -> Result<Value> {
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
}

#[derive(Debug, Clone, Copy)]
pub struct Options {
    pub move_models: bool,
    pub migrate_quickshell: bool,
    pub start_daemon: bool,
    pub disable_hyprwhspr: bool,
}

#[derive(Debug)]
pub struct Report {
    pub config: PathBuf,
    pub config_backup: Option<PathBuf>,
    pub models: Vec<ModelImport>,
    pub quickshell_service: Option<PathBuf>,
    pub notes: Vec<Note>,
}

pub async fn run_hyprwhspr(options: Options) -> Result<Report> {
    let source_path = hyprwhspr_config_path();
    let source = read_hyprwhspr_config(&source_path)?;
    let config_path = paths::config_file();
    let base = Config::load().context("loading the existing Duskr configuration")?;
    let (mut config, mut notes) = translate(&source, base);

    let config_backup = backup(&config_path)?;
    let models = import_models(&config.models_dir(), options.move_models)?;

    if config.asr.backend == "whisper"
        && crate::models::find(&config.models_dir(), &config.asr.whisper.model).is_none()
    {
        let legacy_model = as_str(source.get("model"));
        let available = legacy_model
            .filter(|name| crate::models::find(&config.models_dir(), name).is_some())
            .or_else(|| models.first().map(|model| model.name.clone()));
        if let Some(model) = available {
            notes.push(Note::Warning(format!(
                "selected imported model '{model}' because the configured local model was unavailable"
            )));
            config.asr.whisper.model = model;
        }
    }
    config.save()?;

    let mut quickshell_service = None;

    if options.start_daemon {
        let unit = crate::integrations::systemd::install()?;
        notes.push(Note::Mapped {
            from: "daemon".into(),
            to: unit.display().to_string(),
        });
        crate::integrations::systemd::systemctl(&["daemon-reload"]).await?;
        crate::integrations::systemd::systemctl(&[
            "enable",
            crate::integrations::systemd::UNIT_NAME,
        ])
        .await?;
        crate::integrations::systemd::systemctl(&[
            "restart",
            crate::integrations::systemd::UNIT_NAME,
        ])
        .await?;
        crate::cli::wait_for_daemon(std::time::Duration::from_secs(15)).await?;

        if options.migrate_quickshell {
            let detection = quickshell::detect();
            if detection.found() {
                quickshell_service = Some(quickshell::install(&detection)?.service_written);
            }
        }

        if options.disable_hyprwhspr {
            match crate::integrations::systemd::systemctl(&[
                "disable",
                "--now",
                "hyprwhspr.service",
            ])
            .await
            {
                Ok(_) => notes.push(Note::Mapped {
                    from: "hyprwhspr.service".into(),
                    to: "disabled after Duskr startup".into(),
                }),
                Err(error) => notes.push(Note::Warning(format!(
                    "Duskr started, but hyprwhspr.service could not be disabled: {error}"
                ))),
            }
        }
    } else {
        if options.disable_hyprwhspr {
            notes.push(Note::Warning(
                "HyprWhspr was kept enabled because Duskr startup was skipped".into(),
            ));
        }
        if options.migrate_quickshell {
            notes.push(Note::Warning(
                "Quickshell was not changed because Duskr startup was skipped".into(),
            ));
        }
    }

    Ok(Report {
        config: config_path,
        config_backup,
        models,
        quickshell_service,
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn migrate(source: Value) -> (Config, Vec<Note>) {
        translate(&source, Config::default())
    }

    #[test]
    fn the_real_hyprwhspr_config_migrates_end_to_end() {
        let source = json!({
            "recording_mode": "push_to_talk",
            "use_hypr_bindings": true,
            "keyboard_device_names": [
                "GLORIOUS Model O 2 Wireless",
                "Keychron Keychron C3 Pro",
                "Keychron Keychron C3 Pro Keyboard"
            ],
            "model": "medium.en",
            "language": "en",
            "inject_mode": "wtype",
            "transcription_backend": "faster-whisper",
            "faster_whisper_model": "large-v3-turbo",
            "mic_osd_enabled": false
        });
        let (config, notes) = migrate(source);

        assert_eq!(config.general.recording_mode, RecordingMode::PushToTalk);
        assert_eq!(config.general.language.as_deref(), Some("en"));
        assert_eq!(config.shortcuts.device_names.len(), 3);
        assert_eq!(config.asr.backend, "whisper");
        assert_eq!(config.asr.whisper.model, "large-v3-turbo");
        assert!(!config.integrations.osd);

        assert!(notes
            .iter()
            .any(|n| matches!(n, Note::Dropped { key, .. } if key.contains("wtype"))));
        assert!(notes
            .iter()
            .any(|n| matches!(n, Note::Warning(w) if w.contains("use_hypr_bindings"))));
    }

    #[test]
    fn the_pre_recording_mode_boolean_is_still_understood() {
        let (config, _) = migrate(json!({ "push_to_talk": true }));
        assert_eq!(config.general.recording_mode, RecordingMode::PushToTalk);
        let (config, _) = migrate(json!({ "push_to_talk": false }));
        assert_eq!(config.general.recording_mode, RecordingMode::Toggle);
    }

    #[test]
    fn recording_mode_wins_over_the_legacy_boolean() {
        let (config, _) = migrate(json!({ "push_to_talk": true, "recording_mode": "toggle" }));
        assert_eq!(config.general.recording_mode, RecordingMode::Toggle);
    }

    #[test]
    fn a_rest_endpoint_is_reduced_to_its_base_url() {
        let (config, _) = migrate(json!({
            "transcription_backend": "rest-api",
            "rest_endpoint_url": "http://asr.example.test:8787/transcribe",
            "rest_timeout": 20
        }));
        assert_eq!(config.asr.backend, "remote");
        assert_eq!(config.asr.remote.url, "http://asr.example.test:8787");
        assert_eq!(config.asr.remote.timeout_ms, 20_000);
        assert_eq!(config.asr.remote.protocol, RemoteProtocol::Auto);
    }

    #[test]
    fn an_api_key_is_flagged_rather_than_copied() {
        let (config, notes) = migrate(json!({
            "transcription_backend": "rest-api",
            "rest_api_key": "sk-secret"
        }));
        assert_eq!(config.asr.remote.api_key, None);
        assert!(notes
            .iter()
            .any(|n| matches!(n, Note::Warning(w) if w.contains("rest_api_key"))));
    }

    #[test]
    fn text_processing_settings_carry_across() {
        let (config, _) = migrate(json!({
            "word_overrides": { "hyper whisper": "duskr" },
            "filter_filler_words": true,
            "filler_words": ["uh", "erm"],
            "symbol_replacements": false,
            "post_transcription_hook": "tee /tmp/log",
            "auto_submit": true
        }));
        assert_eq!(
            config.text.word_overrides.get("hyper whisper").unwrap(),
            "duskr"
        );
        assert!(config.text.filter_filler_words);
        assert_eq!(config.text.filler_words, vec!["uh", "erm"]);
        assert!(!config.text.symbol_replacements);
        assert_eq!(config.text.post_hook.as_deref(), Some("tee /tmp/log"));
        assert!(config.general.auto_submit);
    }

    #[test]
    fn per_application_rules_are_normalized_and_translated() {
        let (config, _) = migrate(json!({
            "applications": {
                "Emacs": { "auto_paste": "ctrl+y" },
                "KeePassXC": { "auto_paste": false }
            }
        }));
        let emacs = config.input.applications.get("emacs").expect("emacs rule");
        assert_eq!(emacs.paste_chord.as_deref(), Some("ctrl+y"));
        assert_eq!(emacs.mode, Some(crate::core::config::InjectMode::Clipboard));
        assert!(config.input.applications.get("keepassxc").unwrap().disabled);
    }

    #[test]
    fn shortcuts_and_audio_settings_carry_across() {
        let (config, _) = migrate(json!({
            "primary_shortcut": "SUPER+ALT+D",
            "secondary_shortcut": "SUPER+ALT+I",
            "secondary_language": "it",
            "cancel_shortcut": "SUPER+ESCAPE",
            "grab_keys": true,
            "audio_ducking": true,
            "audio_ducking_percent": 70,
            "audio_feedback": true,
            "silence_timeout": 3.5
        }));
        assert_eq!(config.shortcuts.primary, "SUPER+ALT+D");
        assert_eq!(config.shortcuts.secondary_language.as_deref(), Some("it"));
        assert_eq!(config.shortcuts.cancel.as_deref(), Some("SUPER+ESCAPE"));
        assert!(config.shortcuts.grab_keys);
        assert!(config.audio.ducking);
        assert_eq!(config.audio.ducking_percent, 70);
        assert_eq!(config.audio.silence_timeout, 3.5);
    }

    #[test]
    fn an_empty_config_leaves_the_defaults_alone() {
        let defaults = Config::default();
        let (config, notes) = migrate(json!({}));
        assert_eq!(
            config.general.recording_mode,
            defaults.general.recording_mode
        );
        assert_eq!(config.shortcuts.primary, defaults.shortcuts.primary);
        assert!(notes.is_empty());
    }

    #[test]
    fn an_unrecognized_backend_keeps_the_current_one_and_warns() {
        let (config, notes) = migrate(json!({ "transcription_backend": "cohere-transcribe" }));
        assert_eq!(config.asr.backend, "whisper");
        assert!(notes
            .iter()
            .any(|n| matches!(n, Note::Warning(w) if w.contains("cohere"))));
    }

    #[test]
    fn backups_are_timestamped_and_leave_the_original_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, b"original").unwrap();

        let backup = backup(&path).unwrap().expect("backup created");
        assert!(backup.exists());
        assert_eq!(std::fs::read(&backup).unwrap(), b"original");
        assert!(path.exists());
        assert!(backup.to_string_lossy().contains("bak-"));
    }

    #[test]
    fn backing_up_a_missing_file_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(backup(&dir.path().join("absent")).unwrap(), None);
    }

    #[test]
    fn the_model_search_never_includes_a_python_virtualenv() {
        for path in model_search_paths() {
            let path = path.to_string_lossy().to_string();
            assert!(!path.contains("venv"), "{path} would pull in a virtualenv");
            assert!(!path.contains("site-packages"), "{path}");
        }
    }
}
