use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

use crate::core::config::{normalize_language, Config, RecordingMode};
use crate::core::paths;
use crate::input::keymap;
use crate::models;

const MODES: &[(RecordingMode, &str)] = &[
    (RecordingMode::Toggle, "press once to start, again to stop"),
    (
        RecordingMode::PushToTalk,
        "record while the shortcut is held",
    ),
    (RecordingMode::Auto, "tap to toggle, hold to talk"),
    (RecordingMode::Continuous, "record until you stop speaking"),
    (RecordingMode::LongForm, "record until you submit"),
];

/// Load the configuration, setting Voxscribe up the first time it runs.
///
/// A fresh install has no `config.toml`, so one is written after walking
/// through the settings that matter. Without a terminal to ask on nothing is
/// asked and plain defaults are written, which is what happens under systemd.
pub async fn ensure() -> Result<Config> {
    let path = paths::config_file();
    if path.exists() {
        return Ok(Config::load_or_default());
    }

    let mut config = Config::default();
    let model = interview(&mut config).await;

    // Saved before the download so an interrupted fetch still leaves the
    // answers recorded; `voxscribe model download` picks up the partial file.
    config.save().context("writing the initial configuration")?;
    tracing::info!("first run: created {}", path.display());

    if let Some(model) = model {
        let dir = config.models_dir();
        if models::find(&dir, &model).is_none() {
            download(&config, &model, &dir).await;
        }
    }

    Ok(config)
}

/// Fill in `config` from the answers. Returns the model to make sure of, or
/// `None` when there was nothing to ask on and the defaults stand.
async fn interview(config: &mut Config) -> Option<String> {
    if unsafe { libc::isatty(libc::STDIN_FILENO) } != 1 {
        tracing::warn!(
            "first run: writing defaults; run `voxscribe` in a terminal to set Voxscribe up, or edit {}",
            paths::config_file().display()
        );
        return None;
    }

    eprintln!("Voxscribe first run. Press enter to accept the default in brackets.\n");

    let model = ask_model(config).await;
    ask_mode(config).await;
    ask_shortcut(config).await;
    ask_language(config).await;
    ask_translation(config).await;
    ask_switches(config).await;

    eprintln!();
    model
}

async fn ask_model(config: &mut Config) -> Option<String> {
    let installed = models::list(&config.models_dir());
    eprintln!("Speech model:");
    for (index, (name, description)) in models::CATALOG.iter().enumerate() {
        let mark = if installed.iter().any(|entry| entry.name == *name) {
            "  installed"
        } else {
            ""
        };
        eprintln!("{:>3}  {name:<16} {description}{mark}", index + 1);
    }

    // An already downloaded model beats pulling a new one.
    let default = installed
        .iter()
        .find(|entry| models::is_catalog_model(&entry.name))
        .map(|entry| entry.name.clone())
        .unwrap_or_else(|| Config::default().asr.whisper.model);

    let answer = ask("model", &default).await?;
    let model = match answer.parse::<usize>() {
        Ok(index) => models::CATALOG
            .get(index.checked_sub(1)?)
            .map(|(name, _)| (*name).to_string())?,
        Err(_) if models::is_catalog_model(&answer) => answer,
        Err(_) => {
            eprintln!("unknown model '{answer}', using {default}");
            default
        }
    };
    config.asr.whisper.model = model.clone();
    Some(model)
}

async fn ask_mode(config: &mut Config) -> Option<()> {
    eprintln!("\nRecording mode:");
    for (index, (mode, description)) in MODES.iter().enumerate() {
        eprintln!("{:>3}  {:<16} {description}", index + 1, mode.as_str());
    }

    let default = config.general.recording_mode;
    let answer = ask("mode", default.as_str()).await?;
    let mode = match answer.parse::<usize>() {
        Ok(index) => index
            .checked_sub(1)
            .and_then(|index| MODES.get(index))
            .map(|(mode, _)| *mode),
        Err(_) => RecordingMode::parse(&answer),
    };
    match mode {
        Some(mode) => config.general.recording_mode = mode,
        None => eprintln!("unknown mode '{answer}', using {}", default.as_str()),
    }
    Some(())
}

async fn ask_shortcut(config: &mut Config) -> Option<()> {
    eprintln!("\nShortcut that starts recording, for example SUPER+ALT+D.");
    loop {
        let answer = ask("shortcut", &config.shortcuts.primary).await?;
        match keymap::parse_chord(&answer) {
            Ok(_) => {
                config.shortcuts.primary = answer;
                return Some(());
            }
            Err(err) => eprintln!("{err}"),
        }
    }
}

async fn ask_language(config: &mut Config) -> Option<()> {
    eprintln!("\nLanguage you speak, as a code like en, es or pt-BR. 'auto' detects it.");
    let default = config
        .general
        .language
        .clone()
        .unwrap_or_else(|| "auto".into());
    loop {
        let answer = ask("language", &default).await?;
        if answer.eq_ignore_ascii_case("auto") {
            config.general.language = None;
            return Some(());
        }
        match normalize_language(Some(&answer)) {
            Some(code) => {
                config.general.language = Some(code);
                return Some(());
            }
            None => eprintln!("'{answer}' is not a language code"),
        }
    }
}

async fn ask_translation(config: &mut Config) -> Option<()> {
    eprintln!("\nTranslate transcripts before they are typed? A language code turns it on.");
    loop {
        let answer = ask("translate into", "off").await?;
        if matches!(answer.to_ascii_lowercase().as_str(), "off" | "no" | "none") {
            config.translation.target = None;
            return Some(());
        }
        match normalize_language(Some(&answer)) {
            Some(code) => {
                config.translation.target = Some(code);
                return Some(());
            }
            None => eprintln!("'{answer}' is not a language code"),
        }
    }
}

async fn ask_switches(config: &mut Config) -> Option<()> {
    eprintln!();
    config.general.auto_submit =
        ask_yes_no("press enter after typing", config.general.auto_submit).await?;
    config.integrations.osd =
        ask_yes_no("show the on-screen indicator", config.integrations.osd).await?;
    config.integrations.notifications = ask_yes_no(
        "send desktop notifications",
        config.integrations.notifications,
    )
    .await?;
    Some(())
}

async fn ask(label: &str, default: &str) -> Option<String> {
    let answer = prompt(&format!("{label} [{default}]: ")).await?;
    let answer = answer.trim();
    Some(if answer.is_empty() {
        default.to_string()
    } else {
        answer.to_string()
    })
}

async fn ask_yes_no(label: &str, default: bool) -> Option<bool> {
    let hint = if default { "Y/n" } else { "y/N" };
    loop {
        let answer = prompt(&format!("{label}? [{hint}]: ")).await?;
        match answer.trim().to_ascii_lowercase().as_str() {
            "" => return Some(default),
            "y" | "yes" => return Some(true),
            "n" | "no" => return Some(false),
            other => eprintln!("answer y or n, not '{other}'"),
        }
    }
}

/// Read one line. `None` means the input ended, so the rest is left at its
/// default rather than asking into a closed pipe.
async fn prompt(label: &str) -> Option<String> {
    let label = label.to_string();
    tokio::task::spawn_blocking(move || {
        let mut line = String::new();
        eprint!("{label}");
        std::io::stderr().flush().ok();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line),
        }
    })
    .await
    .ok()
    .flatten()
}

async fn download(config: &Config, model: &str, dir: &Path) {
    eprintln!("downloading {model} into {}", dir.display());
    let mut last_step = u64::MAX;
    let result = models::download::download(
        &config.models.download_base_url,
        model,
        dir,
        false,
        |progress| {
            if let Some(fraction) = progress.fraction() {
                let step = (fraction * 20.0) as u64;
                if step != last_step {
                    last_step = step;
                    eprint!("\r{:3}%", step * 5);
                }
            }
        },
    )
    .await;
    if last_step != u64::MAX {
        eprintln!();
    }

    match result {
        Ok(path) => tracing::info!("first run: model ready at {}", path.display()),
        Err(err) => tracing::warn!(
            "could not download {model}: {err:#}; run `voxscribe model download {model}` to retry"
        ),
    }
}
