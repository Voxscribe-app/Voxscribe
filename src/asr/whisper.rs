//! Local whisper.cpp backend.
//!
//! Reads the same ggml files hyprwhspr and pywhispercpp use, so a migrated
//! `medium.en` keeps working without re-downloading anything. The model is
//! loaded once and kept resident; inference runs on a blocking thread so the
//! async runtime stays free for IPC and state updates.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::core::config::Config;

/// Resolve a model reference to a file on disk.
///
/// Accepts an absolute path, a bare name (`medium.en`), or an already-prefixed
/// file name (`ggml-medium.en.bin`).
pub fn resolve_model_path(model: &str, models_dir: &Path) -> PathBuf {
    let model = model.trim();
    let candidate = Path::new(model);
    if candidate.is_absolute() {
        return candidate.to_path_buf();
    }
    if model.ends_with(".bin") {
        return models_dir.join(model);
    }
    models_dir.join(format!("ggml-{model}.bin"))
}

/// Model name as users refer to it, derived from a ggml file name.
pub fn model_name_from_path(path: &Path) -> String {
    let stem = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    stem.strip_prefix("ggml-")
        .unwrap_or(stem)
        .strip_suffix(".bin")
        .unwrap_or(stem)
        .to_string()
}

#[cfg(feature = "whisper")]
mod imp {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    use anyhow::{bail, Context, Result};
    use async_trait::async_trait;
    use tokio::sync::Mutex;
    use whisper_rs::{
        FullParams, SamplingStrategy as WhisperStrategy, WhisperContext, WhisperContextParameters,
        WhisperState,
    };

    use crate::asr::{Backend, BackendInfo, TranscribeRequest, Transcript};
    use crate::core::config::{Config, SamplingStrategy};

    /// whisper.cpp is fixed at 16 kHz.
    const MODEL_SAMPLE_RATE: u32 = 16_000;

    struct Loaded {
        _context: Arc<WhisperContext>,
        state: WhisperState,
    }

    pub struct WhisperBackend {
        model_path: PathBuf,
        model_name: String,
        threads: usize,
        prompt: String,
        translate: bool,
        beam_size: usize,
        strategy: SamplingStrategy,
        temperature: f32,
        suppress_non_speech: bool,
        use_gpu: bool,
        loaded: Mutex<Option<Loaded>>,
        ready: AtomicBool,
    }

    impl WhisperBackend {
        pub fn new(config: &Config) -> Result<Self> {
            let model_path =
                super::resolve_model_path(&config.asr.whisper.model, &config.models_dir());
            Ok(Self {
                model_name: super::model_name_from_path(&model_path),
                model_path,
                threads: config.whisper_threads(),
                prompt: config.asr.whisper.prompt.clone(),
                translate: config.asr.whisper.translate,
                beam_size: config.asr.whisper.beam_size.max(1),
                strategy: config.asr.whisper.strategy,
                temperature: config.asr.whisper.temperature,
                suppress_non_speech: config.asr.whisper.suppress_non_speech,
                use_gpu: config.asr.whisper.use_gpu,
                loaded: Mutex::new(None),
                ready: AtomicBool::new(false),
            })
        }

        fn open(&self) -> Result<Loaded> {
            if !self.model_path.exists() {
                bail!(
                    "model '{}' not found at {} - run `duskr model download {}`",
                    self.model_name,
                    self.model_path.display(),
                    self.model_name
                );
            }
            let mut params = WhisperContextParameters::default();
            params.use_gpu(self.use_gpu);

            let path = self
                .model_path
                .to_str()
                .context("model path is not valid UTF-8")?;
            let context = Arc::new(
                WhisperContext::new_with_params(path, params)
                    .with_context(|| format!("loading {}", self.model_path.display()))?,
            );
            let state = context
                .create_state()
                .context("creating the whisper decoding state")?;
            Ok(Loaded {
                _context: context,
                state,
            })
        }

        fn params<'a>(&'a self, language: Option<&'a str>) -> FullParams<'a, 'a> {
            let strategy = match self.strategy {
                SamplingStrategy::Greedy => WhisperStrategy::Greedy { best_of: 1 },
                SamplingStrategy::BeamSearch => WhisperStrategy::BeamSearch {
                    beam_size: self.beam_size as i32,
                    patience: -1.0,
                },
            };
            let mut params = FullParams::new(strategy);
            params.set_n_threads(self.threads as i32);
            params.set_translate(self.translate);
            params.set_temperature(self.temperature);
            params.set_suppress_blank(true);
            params.set_suppress_nst(self.suppress_non_speech);
            // Nothing consumes timestamps, and printing anything would land in
            // the journal on every dictation.
            params.set_no_timestamps(true);
            params.set_print_special(false);
            params.set_print_progress(false);
            params.set_print_realtime(false);
            params.set_print_timestamps(false);
            // Each dictation is independent; carrying context between them makes
            // whisper repeat the previous transcript when the audio is quiet.
            params.set_no_context(true);
            if !self.prompt.trim().is_empty() {
                params.set_initial_prompt(&self.prompt);
            }
            if let Some(language) = language.filter(|l| !l.is_empty()) {
                params.set_language(Some(language));
            }
            params
        }
    }

    #[async_trait]
    impl Backend for WhisperBackend {
        fn info(&self) -> BackendInfo {
            BackendInfo {
                id: "whisper".into(),
                model: Some(self.model_name.clone()),
                local: true,
                description: format!("whisper.cpp ({})", self.model_path.display()),
            }
        }

        async fn load(&self) -> Result<()> {
            let mut guard = self.loaded.lock().await;
            if guard.is_some() {
                return Ok(());
            }
            // Loading a large model is seconds of CPU and gigabytes of I/O;
            // keeping it off the runtime keeps IPC answering meanwhile.
            let loaded = tokio::task::block_in_place(|| self.open())?;
            *guard = Some(loaded);
            self.ready.store(true, Ordering::SeqCst);
            tracing::info!("whisper model '{}' loaded", self.model_name);
            Ok(())
        }

        async fn transcribe(&self, request: TranscribeRequest<'_>) -> Result<Transcript> {
            let samples = crate::audio::wav::resample(
                request.samples,
                request.sample_rate,
                MODEL_SAMPLE_RATE,
            );

            let started = Instant::now();
            let mut guard = self.loaded.lock().await;
            if guard.is_none() {
                drop(guard);
                self.load().await?;
                guard = self.loaded.lock().await;
            }
            let loaded = guard.as_mut().expect("model loaded above");

            let params = self.params(request.language);
            let text = tokio::task::block_in_place(|| -> Result<String> {
                loaded
                    .state
                    .full(params, &samples)
                    .context("running whisper inference")?;

                let mut text = String::new();
                for index in 0..loaded.state.full_n_segments() {
                    if let Some(segment) = loaded.state.get_segment(index) {
                        text.push_str(&segment.to_str_lossy()?);
                    }
                }
                Ok(text)
            })?;

            Ok(Transcript {
                text: text.trim().to_string(),
                latency: started.elapsed(),
                backend: "whisper".into(),
                model: Some(self.model_name.clone()),
            })
        }

        fn is_ready(&self) -> bool {
            self.ready.load(Ordering::SeqCst)
        }

        async fn unload(&self) -> Result<()> {
            *self.loaded.lock().await = None;
            self.ready.store(false, Ordering::SeqCst);
            tracing::info!("whisper model unloaded");
            Ok(())
        }
    }
}

#[cfg(feature = "whisper")]
pub fn build(config: &Config) -> Result<Box<dyn crate::asr::Backend>> {
    Ok(Box::new(imp::WhisperBackend::new(config)?))
}

#[cfg(not(feature = "whisper"))]
pub fn build(_config: &Config) -> Result<Box<dyn crate::asr::Backend>> {
    anyhow::bail!("this build has no local whisper support; rebuild with --features whisper")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_model_name_resolves_to_a_ggml_file_in_the_model_directory() {
        let dir = Path::new("/models");
        assert_eq!(
            resolve_model_path("medium.en", dir),
            PathBuf::from("/models/ggml-medium.en.bin")
        );
    }

    #[test]
    fn an_absolute_path_is_used_verbatim() {
        assert_eq!(
            resolve_model_path("/elsewhere/ggml-tiny.bin", Path::new("/models")),
            PathBuf::from("/elsewhere/ggml-tiny.bin")
        );
    }

    #[test]
    fn an_explicit_file_name_is_not_prefixed_twice() {
        assert_eq!(
            resolve_model_path("ggml-large-v3.bin", Path::new("/models")),
            PathBuf::from("/models/ggml-large-v3.bin")
        );
    }

    #[test]
    fn model_names_round_trip_through_their_file_names() {
        for name in ["medium.en", "large-v3-turbo", "tiny"] {
            let path = resolve_model_path(name, Path::new("/models"));
            assert_eq!(model_name_from_path(&path), name);
        }
    }

    #[test]
    fn a_non_ggml_file_name_is_reported_as_is() {
        assert_eq!(
            model_name_from_path(Path::new("/models/custom-model.bin")),
            "custom-model"
        );
    }
}
