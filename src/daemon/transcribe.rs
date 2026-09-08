use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

use crate::asr::{Backend, TranscribeRequest};
use crate::core::config::Config;
use crate::core::state::{Event, Phase, StateHandle};
use crate::input::inject::Injector;
use crate::text;
use crate::translate;

pub struct Job {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub language: Option<String>,
    pub config: Arc<Config>,
    pub keep_recording: bool,
}

pub struct Worker {
    pub backend: Arc<dyn Backend>,
    pub injector: Option<Arc<Injector>>,
    pub state: StateHandle,
}

impl Worker {
    pub async fn run(self, mut jobs: mpsc::UnboundedReceiver<Job>) {
        while let Some(job) = jobs.recv().await {
            let keep_recording = job.keep_recording;
            match self.process(job).await {
                Ok(text) => {
                    self.state.update(|snapshot| {
                        if !keep_recording {
                            snapshot.phase = Phase::Idle;
                            snapshot.message = "ready".into();
                        }
                        if !text.is_empty() {
                            snapshot.last_transcript = Some(text.clone());
                        }
                    });
                }
                Err(err) => {
                    let message = format!("{err:#}");
                    tracing::warn!("transcription failed: {message}");
                    self.state.emit(Event::Error {
                        message: message.clone(),
                    });
                    self.state.update(|snapshot| {
                        snapshot.phase = Phase::Error;
                        snapshot.message = message.clone();
                    });
                }
            }
        }
    }

    async fn process(&self, job: Job) -> Result<String> {
        let started = Instant::now();
        crate::asr::validate_audio(&job.samples, job.sample_rate)?;

        let transcript = self
            .backend
            .transcribe(TranscribeRequest {
                samples: &job.samples,
                sample_rate: job.sample_rate,
                language: job.language.as_deref(),
                prompt: Some(job.config.asr.whisper.prompt.as_str()),
            })
            .await?;

        self.state.update(|snapshot| {
            snapshot.last_latency_ms = Some(transcript.latency.as_millis() as u64);
            snapshot.backend = transcript.backend.clone();
            snapshot.model = transcript.model.clone();
            snapshot.ready = true;
        });

        let text = finish(
            &transcript.text,
            &job.config,
            &transcript.backend,
            Some(&self.state),
        )
        .await;
        if text.is_empty() {
            tracing::debug!("nothing to inject after processing");
            return Ok(String::new());
        }

        if let Err(error) = record_history(&text, &transcript, &job.config).await {
            tracing::warn!("could not update transcript history: {error:#}");
        }
        let _ = tokio::fs::write(crate::core::paths::transcript_preview_file(), &text).await;

        self.state.emit(Event::Transcript {
            text: text.clone(),
            final_: true,
        });

        if let Some(injector) = &self.injector {
            let injector = Arc::clone(injector);
            let config = Arc::clone(&job.config);
            let payload = text.clone();
            let outcome =
                tokio::task::spawn_blocking(move || injector.inject(&payload, &config)).await??;
            tracing::info!(
                "injected {} chars via {:?} in {:?}",
                outcome.chars,
                outcome.mode,
                started.elapsed()
            );
            if !outcome.fallback_chars.is_empty() {
                tracing::debug!(
                    "used the clipboard for unrepresentable characters: {:?}",
                    outcome.fallback_chars
                );
            }
            self.state.emit(Event::Injected { text: text.clone() });
        }

        Ok(text)
    }
}

#[derive(Serialize)]
struct HistoryEntry<'a> {
    timestamp: String,
    text: &'a str,
    backend: &'a str,
    model: Option<&'a str>,
    latency_ms: u64,
}

async fn record_history(
    text: &str,
    transcript: &crate::asr::Transcript,
    config: &Config,
) -> Result<()> {
    let limit = config.general.history_limit;
    if limit == 0 {
        return Ok(());
    }
    let path = crate::core::paths::history_file();
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let entry = HistoryEntry {
        timestamp: chrono::Local::now().to_rfc3339(),
        text: text.trim_end(),
        backend: &transcript.backend,
        model: transcript.model.as_deref(),
        latency_ms: transcript.latency.as_millis() as u64,
    };
    let mut line = serde_json::to_vec(&entry)?;
    line.push(b'\n');
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await?;
    file.write_all(&line).await?;
    file.flush().await?;

    let raw = tokio::fs::read_to_string(&path).await?;
    let trimmed = trim_history(&raw, limit);
    if trimmed.len() != raw.len() {
        crate::core::paths::write_atomic(&path, trimmed.as_bytes())?;
    }
    Ok(())
}

fn trim_history(raw: &str, limit: usize) -> String {
    let lines = raw.lines().collect::<Vec<_>>();
    let start = lines.len().saturating_sub(limit);
    let mut output = lines[start..].join("\n");
    if !output.is_empty() {
        output.push('\n');
    }
    output
}

pub async fn finish(
    raw: &str,
    config: &Config,
    backend: &str,
    state: Option<&StateHandle>,
) -> String {
    let processed = text::process(raw, &config.text);
    let outcome = if processed.is_empty() {
        translate::Outcome::empty()
    } else {
        translate::apply(&processed, config).await
    };

    if let Some(state) = state {
        state.emit(Event::Pipeline {
            raw: raw.to_string(),
            processed: processed.clone(),
            translated: outcome.text.clone(),
            source: outcome.source,
            target: outcome.target,
        });
    }

    let translated = outcome.text;
    if translated.is_empty() {
        return String::new();
    }

    let hooked = match &config.text.post_hook {
        Some(command) if !command.trim().is_empty() => {
            text::hook::run(
                command,
                &translated,
                std::time::Duration::from_millis(config.text.post_hook_timeout_ms),
                text::hook::HookContext {
                    backend,
                    model: &config.asr.whisper.model,
                    language: &output_language(config),
                },
            )
            .await
        }
        _ => translated,
    };

    text::finalize_for_injection(&hooked, &config.text)
}

fn output_language(config: &Config) -> String {
    config
        .translation
        .target_code()
        .unwrap_or_else(|| config.general.language.clone().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_pipeline_processes_then_finalizes() {
        let config = Config::default();
        assert_eq!(
            finish("hello world\n", &config, "test", None).await,
            "hello world "
        );
    }

    #[tokio::test]
    async fn a_hallucinated_transcript_produces_nothing_to_inject() {
        let config = Config::default();
        assert_eq!(finish("Thank you.", &config, "test", None).await, "");
    }

    #[tokio::test]
    async fn the_hook_runs_after_processing_and_before_the_trailing_space() {
        let mut config = Config::default();
        config.text.post_hook = Some("tr a-z A-Z".into());
        assert_eq!(finish("hello", &config, "test", None).await, "HELLO ");
    }

    #[tokio::test]
    async fn the_hook_is_skipped_for_an_empty_transcript() {
        let mut config = Config::default();
        config.text.post_hook = Some("echo replaced".into());
        assert_eq!(finish("Thank you.", &config, "test", None).await, "");
    }

    #[test]
    fn history_keeps_only_the_newest_entries() {
        assert_eq!(trim_history("one\ntwo\nthree\n", 2), "two\nthree\n");
        assert_eq!(trim_history("one\n", 2), "one\n");
    }
}
