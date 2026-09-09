use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use parakeet_rs::{ExecutionConfig, ExecutionProvider, ParakeetUnified, Transcriber};
use serde::Serialize;
use tokio::io::AsyncWriteExt;

use crate::{Transcription, SAMPLE_RATE};

pub const DEFAULT_REPOSITORY: &str = "bobNight/parakeet-unified-en-0.6b-onnx";
pub const MODEL_NAME: &str = "parakeet-unified-en-0.6b";

const MODEL_FILES: [&str; 4] = [
    "encoder.onnx",
    "encoder.onnx.data",
    "decoder_joint.onnx",
    "tokenizer.model",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Cpu,
    Cuda,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Cuda => "cuda",
        }
    }
}

pub fn default_model_dir() -> Result<PathBuf> {
    Ok(dirs::data_dir()
        .context("XDG data directory is unavailable")?
        .join("duskr/server/models")
        .join(MODEL_NAME))
}

pub fn validate_model_dir(path: &Path) -> Result<()> {
    let missing = MODEL_FILES
        .iter()
        .filter(|name| !path.join(name).is_file())
        .copied()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        bail!(
            "model directory {} is missing: {}",
            path.display(),
            missing.join(", ")
        );
    }
    Ok(())
}

pub async fn setup(model_dir: &Path, repository: &str, force: bool) -> Result<()> {
    if repository.chars().any(char::is_whitespace) || repository.contains("..") {
        bail!("invalid Hugging Face repository name");
    }
    tokio::fs::create_dir_all(model_dir).await?;
    let client = reqwest::Client::builder().build()?;
    for name in MODEL_FILES {
        let destination = model_dir.join(name);
        if destination.is_file() && !force {
            tracing::info!(file = %destination.display(), "already downloaded");
            continue;
        }
        let url = format!("https://huggingface.co/{repository}/resolve/main/{name}");
        let partial = model_dir.join(format!(".{name}.partial"));
        tracing::info!(%url, "downloading model file");
        let response = client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("requesting {url}"))?
            .error_for_status()
            .with_context(|| format!("downloading {url}"))?;
        let mut output = tokio::fs::File::create(&partial).await?;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            output.write_all(&chunk?).await?;
        }
        output.flush().await?;
        drop(output);
        tokio::fs::rename(&partial, &destination).await?;
    }
    validate_model_dir(model_dir)?;
    println!("model ready at {}", model_dir.display());
    Ok(())
}

pub struct Engine {
    model: ParakeetUnified,
}

impl Engine {
    pub fn load(model_dir: &Path, provider: Provider) -> Result<Self> {
        validate_model_dir(model_dir)?;
        let config = match provider {
            Provider::Cpu => ExecutionConfig::new().with_execution_provider(ExecutionProvider::Cpu),
            Provider::Cuda => ExecutionConfig::new().with_custom_configure(|builder| {
                Ok(builder.with_execution_providers([ort::ep::CUDA::default()
                    .build()
                    .error_on_failure()])?)
            }),
        };
        let model = ParakeetUnified::from_pretrained(model_dir, Some(config))
            .with_context(|| format!("loading ONNX model from {}", model_dir.display()))?;
        Ok(Self { model })
    }

    pub fn transcribe(&mut self, pcm: &[u8]) -> Result<Transcription> {
        let samples = pcm
            .chunks_exact(2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]) as f32 / 32768.0)
            .collect::<Vec<_>>();
        let audio_seconds = samples.len() as f64 / f64::from(SAMPLE_RATE);
        let started = Instant::now();
        let result = self
            .model
            .transcribe_samples(samples, SAMPLE_RATE, 1, None)?;
        let inference_seconds = started.elapsed().as_secs_f64();
        Ok(Transcription {
            text: result.text,
            audio_seconds,
            inference_seconds,
            realtime_x: (inference_seconds > 0.0).then(|| audio_seconds / inference_seconds),
            peak_allocated_gib: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_layout_is_checked() {
        let path = std::env::temp_dir().join(format!("duskr-server-test-{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        assert!(validate_model_dir(&path).is_err());
        for name in MODEL_FILES {
            std::fs::write(path.join(name), []).unwrap();
        }
        assert!(validate_model_dir(&path).is_ok());
        std::fs::remove_dir_all(path).unwrap();
    }
}
