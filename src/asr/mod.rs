//! Replaceable speech-recognition backends.
//!
//! Everything above this layer speaks only [`Backend`], so adding a provider
//! means adding a module and a registry entry. The trait carries a streaming
//! extension point that returns "unsupported" by default: v1 ships batch-only
//! because the measured round trip is already well under the time it takes a
//! user to release a key, but the shape is here so streaming can be added
//! without reworking callers.

pub mod remote;
pub mod whisper;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use async_trait::async_trait;

use crate::core::config::Config;

#[derive(Debug, Clone, PartialEq)]
pub struct BackendInfo {
    pub id: String,
    pub model: Option<String>,
    /// Runs on this machine, so the model can be loaded and unloaded.
    pub local: bool,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct TranscribeRequest<'a> {
    /// Mono f32 samples in -1.0..=1.0.
    pub samples: &'a [f32],
    pub sample_rate: u32,
    pub language: Option<&'a str>,
    /// Style hint; ignored by backends that have no equivalent.
    pub prompt: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Transcript {
    pub text: String,
    pub latency: Duration,
    pub backend: String,
    pub model: Option<String>,
}

/// Live streaming session. No backend implements this yet; the type exists so
/// that adding one is additive rather than a redesign.
#[async_trait]
pub trait StreamingSession: Send {
    async fn push(&mut self, samples: &[f32]) -> Result<()>;
    /// Interim text since the last call, if the provider offers any.
    async fn partial(&mut self) -> Result<Option<String>>;
    async fn finish(self: Box<Self>) -> Result<Transcript>;
}

#[async_trait]
pub trait Backend: Send + Sync {
    fn info(&self) -> BackendInfo;

    /// Prepare for transcription: load the model, or open the connection.
    async fn load(&self) -> Result<()>;

    async fn transcribe(&self, request: TranscribeRequest<'_>) -> Result<Transcript>;

    fn is_ready(&self) -> bool;

    /// Release the model. Local backends free memory; remote ones no-op.
    async fn unload(&self) -> Result<()> {
        Ok(())
    }

    fn supports_streaming(&self) -> bool {
        false
    }

    async fn open_stream(&self, _language: Option<&str>) -> Result<Box<dyn StreamingSession>> {
        bail!("{} does not support streaming", self.info().id)
    }
}

/// Backend ids accepted by the config, with their aliases.
pub const KNOWN_BACKENDS: &[(&str, &str)] = &[
    (
        "whisper",
        "Local whisper.cpp; reads ggml models, including hyprwhspr's",
    ),
    (
        "remote",
        "Remote HTTP backend (Parakeet and compatible servers)",
    ),
    (
        "none",
        "Accept audio and produce nothing; for testing the pipeline",
    ),
];

pub fn canonical_backend_id(id: &str) -> &str {
    match id.trim().to_ascii_lowercase().as_str() {
        // hyprwhspr's local-backend spellings all mean "run whisper.cpp here".
        "whisper" | "whisper.cpp" | "whisper-rs" | "pywhispercpp" | "cpu" | "nvidia" | "vulkan"
        | "amd" | "faster-whisper" => "whisper",
        "remote" | "parakeet" | "rest-api" | "rest" | "http" | "onnx-asr" => "remote",
        "none" | "null" | "noop" => "none",
        _ => "unknown",
    }
}

/// Build the configured backend.
pub fn build(config: &Config) -> Result<Box<dyn Backend>> {
    let mut ids = vec![config.asr.backend.as_str()];
    for id in &config.asr.fallback {
        if !ids
            .iter()
            .any(|known| canonical_backend_id(known) == canonical_backend_id(id))
        {
            ids.push(id);
        }
    }
    if ids.len() == 1 {
        return build_id(ids[0], config);
    }
    let candidates = ids
        .into_iter()
        .map(|id| build_id(id, config).map(Arc::<dyn Backend>::from))
        .collect::<Result<Vec<_>>>()?;
    Ok(Box::new(FallbackBackend::new(candidates)))
}

pub fn build_id(id: &str, config: &Config) -> Result<Box<dyn Backend>> {
    match canonical_backend_id(id) {
        "whisper" => whisper::build(config),
        "remote" => Ok(Box::new(remote::RemoteBackend::new(&config.asr.remote)?)),
        "none" => Ok(Box::new(NullBackend)),
        _ => bail!(
            "unknown ASR backend '{id}'; known backends: {}",
            KNOWN_BACKENDS
                .iter()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Consumes audio and returns nothing. Useful for exercising the capture and
/// injection paths without loading a model.
pub struct NullBackend;

struct FallbackBackend {
    candidates: Vec<Arc<dyn Backend>>,
    active: AtomicUsize,
    ready: AtomicBool,
}

impl FallbackBackend {
    fn new(candidates: Vec<Arc<dyn Backend>>) -> Self {
        Self {
            candidates,
            active: AtomicUsize::new(0),
            ready: AtomicBool::new(false),
        }
    }
}

#[async_trait]
impl Backend for FallbackBackend {
    fn info(&self) -> BackendInfo {
        self.candidates[self.active.load(Ordering::Relaxed)].info()
    }

    async fn load(&self) -> Result<()> {
        let mut errors = Vec::new();
        for (index, backend) in self.candidates.iter().enumerate() {
            match backend.load().await {
                Ok(()) => {
                    self.active.store(index, Ordering::SeqCst);
                    self.ready.store(true, Ordering::SeqCst);
                    return Ok(());
                }
                Err(error) => errors.push(format!("{}: {error:#}", backend.info().id)),
            }
        }
        self.ready.store(false, Ordering::SeqCst);
        bail!("all ASR backends failed: {}", errors.join("; "))
    }

    async fn transcribe(&self, request: TranscribeRequest<'_>) -> Result<Transcript> {
        let start = self.active.load(Ordering::SeqCst);
        let mut errors = Vec::new();
        for offset in 0..self.candidates.len() {
            let index = (start + offset) % self.candidates.len();
            let backend = &self.candidates[index];
            if offset > 0 {
                if let Err(error) = backend.load().await {
                    errors.push(format!("{} load: {error:#}", backend.info().id));
                    continue;
                }
            }
            match backend.transcribe(request.clone()).await {
                Ok(transcript) => {
                    self.active.store(index, Ordering::SeqCst);
                    self.ready.store(true, Ordering::SeqCst);
                    return Ok(transcript);
                }
                Err(error) => errors.push(format!("{}: {error:#}", backend.info().id)),
            }
        }
        self.ready.store(false, Ordering::SeqCst);
        bail!("all ASR backends failed: {}", errors.join("; "))
    }

    fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    async fn unload(&self) -> Result<()> {
        for backend in &self.candidates {
            backend.unload().await?;
        }
        self.ready.store(false, Ordering::SeqCst);
        Ok(())
    }
}

#[async_trait]
impl Backend for NullBackend {
    fn info(&self) -> BackendInfo {
        BackendInfo {
            id: "none".into(),
            model: None,
            local: true,
            description: "no-op backend".into(),
        }
    }

    async fn load(&self) -> Result<()> {
        Ok(())
    }

    async fn transcribe(&self, _request: TranscribeRequest<'_>) -> Result<Transcript> {
        Ok(Transcript {
            text: String::new(),
            latency: Duration::ZERO,
            backend: "none".into(),
            model: None,
        })
    }

    fn is_ready(&self) -> bool {
        true
    }
}

/// Reject audio that cannot possibly transcribe, before paying for a backend
/// round trip.
pub fn validate_audio(samples: &[f32], sample_rate: u32) -> Result<()> {
    if samples.is_empty() {
        bail!("no audio captured");
    }
    let minimum = (sample_rate as f32 * 0.1) as usize;
    if samples.len() < minimum {
        bail!(
            "recording too short ({:.0} ms)",
            samples.len() as f32 / sample_rate as f32 * 1000.0
        );
    }
    if samples.iter().any(|s| !s.is_finite()) {
        bail!("audio contains non-finite samples");
    }
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
    if rms < 1e-6 {
        bail!("audio is silent");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hyprwhspr_backend_names_map_onto_duskr_backends() {
        assert_eq!(canonical_backend_id("pywhispercpp"), "whisper");
        assert_eq!(canonical_backend_id("faster-whisper"), "whisper");
        assert_eq!(canonical_backend_id("NVIDIA"), "whisper");
        assert_eq!(canonical_backend_id("rest-api"), "remote");
        assert_eq!(canonical_backend_id("parakeet"), "remote");
        assert_eq!(canonical_backend_id("something-else"), "unknown");
    }

    #[test]
    fn an_unknown_backend_is_rejected_with_the_known_list() {
        let mut config = Config::default();
        config.asr.backend = "wat".into();
        let err = build(&config)
            .err()
            .expect("unknown backend must fail")
            .to_string();
        assert!(err.contains("wat"), "{err}");
        assert!(err.contains("whisper"), "{err}");
    }

    #[test]
    fn silence_and_stubs_are_rejected_before_reaching_a_backend() {
        assert!(validate_audio(&[], 16_000).is_err());
        assert!(validate_audio(&[0.1; 100], 16_000).is_err(), "too short");
        assert!(validate_audio(&[0.0; 32_000], 16_000).is_err(), "silent");
        assert!(validate_audio(&[f32::NAN; 32_000], 16_000).is_err(), "NaN");
        assert!(validate_audio(&[0.1; 32_000], 16_000).is_ok());
    }

    #[tokio::test]
    async fn the_null_backend_accepts_audio_and_returns_nothing() {
        let backend = NullBackend;
        assert!(backend.is_ready());
        let transcript = backend
            .transcribe(TranscribeRequest {
                samples: &[0.1; 16_000],
                sample_rate: 16_000,
                language: None,
                prompt: None,
            })
            .await
            .unwrap();
        assert_eq!(transcript.text, "");
    }

    #[tokio::test]
    async fn streaming_is_refused_by_name_rather_than_silently() {
        let err = NullBackend
            .open_stream(None)
            .await
            .err()
            .expect("streaming must fail")
            .to_string();
        assert!(err.contains("none"), "{err}");
        assert!(!NullBackend.supports_streaming());
    }
}
