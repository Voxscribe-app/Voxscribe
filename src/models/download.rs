use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

use crate::models::looks_like_model;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Progress {
    pub downloaded: u64,
    pub total: Option<u64>,
}

impl Progress {
    pub fn fraction(&self) -> Option<f64> {
        self.total
            .filter(|total| *total > 0)
            .map(|total| (self.downloaded as f64 / total as f64).min(1.0))
    }
}

pub fn model_url(base_url: &str, model: &str) -> String {
    format!("{}/ggml-{model}.bin", base_url.trim_end_matches('/'))
}

pub async fn download(
    base_url: &str,
    model: &str,
    models_dir: &Path,
    force: bool,
    mut on_progress: impl FnMut(Progress),
) -> Result<PathBuf> {
    let final_path = models_dir.join(format!("ggml-{model}.bin"));
    if final_path.exists() && !force {
        if looks_like_model(&final_path) {
            return Ok(final_path);
        }
        tracing::warn!(
            "{} is not a valid model; re-downloading",
            final_path.display()
        );
        std::fs::remove_file(&final_path).ok();
    }

    tokio::fs::create_dir_all(models_dir)
        .await
        .with_context(|| format!("creating {}", models_dir.display()))?;

    let partial_path = final_path.with_extension("bin.part");
    let already = if force {
        tokio::fs::remove_file(&partial_path).await.ok();
        0
    } else {
        tokio::fs::metadata(&partial_path)
            .await
            .map(|m| m.len())
            .unwrap_or(0)
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3600))
        .build()
        .context("building the download client")?;

    let url = model_url(base_url, model);
    let mut request = client.get(&url);
    if already > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={already}-"));
    }

    let response = request
        .send()
        .await
        .with_context(|| format!("requesting {url}"))?;

    if !response.status().is_success() {
        bail!("downloading {url} failed with {}", response.status());
    }

    let resuming = already > 0 && response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    let start = if resuming { already } else { 0 };
    let total = response.content_length().map(|len| len + start);

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(!resuming)
        .open(&partial_path)
        .await
        .with_context(|| format!("opening {}", partial_path.display()))?;
    if resuming {
        file.seek(std::io::SeekFrom::Start(start)).await?;
    }

    let mut downloaded = start;
    on_progress(Progress { downloaded, total });

    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("reading the download stream")?;
        file.write_all(&chunk)
            .await
            .context("writing the model file")?;
        downloaded += chunk.len() as u64;
        on_progress(Progress { downloaded, total });
    }
    file.flush().await?;
    drop(file);

    if let Some(total) = total {
        let actual = tokio::fs::metadata(&partial_path).await?.len();
        if actual != total {
            bail!("download is incomplete ({actual} of {total} bytes); re-run to resume");
        }
    }
    if !looks_like_model(&partial_path) {
        tokio::fs::remove_file(&partial_path).await.ok();
        bail!("downloaded file is not a ggml model; check models.download_base_url");
    }

    tokio::fs::rename(&partial_path, &final_path)
        .await
        .with_context(|| format!("installing {}", final_path.display()))?;
    Ok(final_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_follow_the_ggml_naming_convention() {
        assert_eq!(
            model_url("https://example.com/base/", "medium.en"),
            "https://example.com/base/ggml-medium.en.bin"
        );
    }

    #[test]
    fn progress_is_a_fraction_only_when_the_total_is_known() {
        assert_eq!(
            Progress {
                downloaded: 50,
                total: Some(200)
            }
            .fraction(),
            Some(0.25)
        );
        assert_eq!(
            Progress {
                downloaded: 50,
                total: None
            }
            .fraction(),
            None
        );
        assert_eq!(
            Progress {
                downloaded: 50,
                total: Some(0)
            }
            .fraction(),
            None
        );
    }

    #[test]
    fn progress_never_reports_more_than_complete() {
        assert_eq!(
            Progress {
                downloaded: 300,
                total: Some(200)
            }
            .fraction(),
            Some(1.0)
        );
    }

    #[tokio::test]
    async fn an_existing_valid_model_is_not_re_downloaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ggml-tiny.bin");
        std::fs::write(&path, b"lmggxxxx").unwrap();

        let result = download(
            "http://127.0.0.1:1/never",
            "tiny",
            dir.path(),
            false,
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(result, path);
    }
}
