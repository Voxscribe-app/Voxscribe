use anyhow::{Context, Result};

use crate::core::config::Config;
use crate::core::paths;
use crate::models;

/// Load the configuration, setting Duskr up the first time it runs.
///
/// A fresh install has no `config.toml`, so one is written and the default
/// Whisper model is fetched; the daemon is then usable without any manual
/// setup. A failed download is not fatal, the daemon reports it and keeps
/// running so `duskr model download` can retry.
pub async fn ensure() -> Result<Config> {
    let path = paths::config_file();
    if path.exists() {
        return Ok(Config::load_or_default());
    }

    let config = Config::default();
    config.save().context("writing the initial configuration")?;
    tracing::info!("first run: created {}", path.display());

    if crate::asr::canonical_backend_id(&config.asr.backend) == "whisper" {
        fetch_model(&config).await;
    }

    Ok(config)
}

async fn fetch_model(config: &Config) {
    let model = &config.asr.whisper.model;
    let dir = config.models_dir();
    if models::find(&dir, model).is_some() || !models::is_catalog_model(model) {
        return;
    }

    eprintln!(
        "Duskr first run: downloading the {model} model into {}",
        dir.display()
    );
    let mut last_step = u64::MAX;
    let result = models::download::download(
        &config.models.download_base_url,
        model,
        &dir,
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
