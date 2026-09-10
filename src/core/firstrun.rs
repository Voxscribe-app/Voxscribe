use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

use crate::core::config::Config;
use crate::core::paths;
use crate::models;

/// Load the configuration, setting Duskr up the first time it runs.
///
/// A fresh install has no `config.toml`, so one is written after asking which
/// Whisper model to use; the daemon is then usable without any manual setup.
/// Without a terminal, or when the download fails, the configuration is still
/// written and `duskr model download` can finish the job later.
pub async fn ensure() -> Result<Config> {
    let path = paths::config_file();
    if path.exists() {
        return Ok(Config::load_or_default());
    }

    let mut config = Config::default();
    let choice = match crate::asr::canonical_backend_id(&config.asr.backend) {
        "whisper" => ask(&models::list(&config.models_dir())).await,
        _ => None,
    };
    if let Some(model) = &choice {
        config.asr.whisper.model = model.clone();
    } else {
        tracing::warn!(
            "first run: no model selected; run `duskr model catalog` then \
             `duskr model download <name>`"
        );
    }

    // Saved before the download so an interrupted fetch still leaves the
    // choice recorded; `duskr model download` picks up the partial file.
    config.save().context("writing the initial configuration")?;
    tracing::info!("first run: created {}", path.display());

    if let Some(model) = choice {
        let dir = config.models_dir();
        if models::find(&dir, &model).is_none() {
            download(&config, &model, &dir).await;
        }
    }

    Ok(config)
}

/// Ask which model to use. Returns `None` when there is no terminal to ask on,
/// which is the usual case under systemd.
async fn ask(installed: &[models::ModelEntry]) -> Option<String> {
    if unsafe { libc::isatty(libc::STDIN_FILENO) } != 1 {
        return None;
    }

    eprintln!("Duskr first run: pick a speech model.\n");
    for (index, (name, description)) in models::CATALOG.iter().enumerate() {
        let mark = if installed.iter().any(|entry| entry.name == *name) {
            "installed"
        } else {
            ""
        };
        eprintln!("{:>3}  {name:<16} {description} {mark}", index + 1);
    }
    eprintln!();

    let default = default_choice(installed);
    let answer = prompt(&format!("model [{default}]: ")).await?;
    let answer = answer.trim();
    if answer.is_empty() {
        return Some(default);
    }
    if let Ok(index) = answer.parse::<usize>() {
        return models::CATALOG
            .get(index.checked_sub(1)?)
            .map(|(name, _)| (*name).to_string());
    }
    if models::is_catalog_model(answer) {
        return Some(answer.to_string());
    }
    eprintln!("unknown model '{answer}', using {default}");
    Some(default)
}

/// Prefer a model that is already downloaded over pulling a new one.
fn default_choice(installed: &[models::ModelEntry]) -> String {
    installed
        .iter()
        .find(|entry| models::is_catalog_model(&entry.name))
        .map(|entry| entry.name.clone())
        .unwrap_or_else(|| Config::default().asr.whisper.model)
}

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
            "could not download {model}: {err:#}; run `duskr model download {model}` to retry"
        ),
    }
}
