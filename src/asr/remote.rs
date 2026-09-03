//! Remote HTTP ASR (Parakeet and compatible servers).
//!
//! The fast path posts raw 16 kHz mono s16le with no container and no
//! base64 - the audio is already in the exact shape the model wants, so
//! writing a temporary WAV would only add work at both ends. Connections are
//! pooled and kept alive, and optionally warmed at daemon start, so a dictation
//! never pays for a TCP handshake.
//!
//! `multipart` remains available for servers that only accept a WAV upload, and
//! `auto` probes once and remembers.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE};
use reqwest::{Client, StatusCode};
use serde::Deserialize;

use crate::asr::{Backend, BackendInfo, TranscribeRequest, Transcript};
use crate::audio::wav;
use crate::core::config::{RemoteConfig, RemoteProtocol};

const PCM_CONTENT_TYPE: &str = "audio/l16; rate=16000; channels=1";
const PCM_SAMPLE_RATE: u32 = 16_000;

/// Resolved wire format, stored as an atom so `auto` can settle itself without
/// a lock on the transcription path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wire {
    Unknown = 0,
    Pcm = 1,
    Multipart = 2,
}

impl Wire {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Wire::Pcm,
            2 => Wire::Multipart,
            _ => Wire::Unknown,
        }
    }
}

#[derive(Deserialize)]
struct TranscriptionResponse {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    transcript: Option<String>,
    #[serde(default)]
    result: Option<String>,
}

impl TranscriptionResponse {
    fn into_text(self) -> Option<String> {
        self.text.or(self.transcript).or(self.result)
    }
}

pub struct RemoteBackend {
    client: Client,
    base_url: String,
    model: Option<String>,
    timeout: Duration,
    warmup: bool,
    wire: AtomicU8,
    ready: AtomicBool,
}

impl RemoteBackend {
    pub fn new(config: &RemoteConfig) -> Result<Self> {
        let mut headers = HeaderMap::new();
        for (name, value) in &config.headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .with_context(|| format!("invalid header name '{name}'"))?;
            let value = HeaderValue::from_str(value)
                .with_context(|| format!("invalid value for header '{name}'"))?;
            headers.insert(name, value);
        }
        if let Some(key) = &config.api_key {
            if !key.is_empty() {
                let mut value =
                    HeaderValue::from_str(&format!("Bearer {key}")).context("invalid API key")?;
                value.set_sensitive(true);
                headers.insert(reqwest::header::AUTHORIZATION, value);
            }
        }

        let timeout = Duration::from_millis(config.timeout_ms.max(1));
        let client = Client::builder()
            .default_headers(headers)
            .timeout(timeout)
            // Latency, not throughput: a half-second of Nagle buffering would
            // dwarf the inference time.
            .tcp_nodelay(true)
            .pool_idle_timeout(Duration::from_secs(600))
            .pool_max_idle_per_host(4)
            .build()
            .context("building the remote ASR HTTP client")?;

        let wire = match config.protocol {
            RemoteProtocol::Auto => Wire::Unknown,
            RemoteProtocol::Pcm => Wire::Pcm,
            RemoteProtocol::Multipart => Wire::Multipart,
        };

        Ok(Self {
            client,
            base_url: config.url.trim_end_matches('/').to_string(),
            model: config.model.clone(),
            timeout,
            warmup: config.warmup,
            wire: AtomicU8::new(wire as u8),
            ready: AtomicBool::new(false),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    fn query(&self, language: Option<&str>) -> Vec<(String, String)> {
        let mut query = Vec::new();
        if let Some(language) = language.filter(|l| !l.is_empty()) {
            query.push(("language".to_string(), language.to_string()));
        }
        if let Some(model) = &self.model {
            query.push(("model".to_string(), model.clone()));
        }
        query
    }

    async fn post_pcm(&self, request: &TranscribeRequest<'_>) -> Result<Option<String>> {
        let samples = resample_for_wire(request.samples, request.sample_rate);
        let body = wav::to_pcm16(&samples);

        let response = self
            .client
            .post(self.url("/transcribe/pcm"))
            .query(&self.query(request.language))
            .header(CONTENT_TYPE, PCM_CONTENT_TYPE)
            .body(body)
            .send()
            .await
            .context("posting PCM to the remote backend")?;

        // A server that does not know this route is telling us to use the
        // compatibility upload instead, not that transcription failed.
        if matches!(
            response.status(),
            StatusCode::NOT_FOUND
                | StatusCode::METHOD_NOT_ALLOWED
                | StatusCode::UNSUPPORTED_MEDIA_TYPE
                | StatusCode::NOT_IMPLEMENTED
        ) {
            return Ok(None);
        }
        Ok(Some(read_text(response).await?))
    }

    async fn post_multipart(&self, request: &TranscribeRequest<'_>) -> Result<String> {
        let samples = resample_for_wire(request.samples, request.sample_rate);
        let wav_bytes = wav::encode(&samples, PCM_SAMPLE_RATE)?;

        let part = reqwest::multipart::Part::bytes(wav_bytes)
            .file_name("audio.wav")
            .mime_str("audio/wav")
            .context("building the audio part")?;
        let mut form = reqwest::multipart::Form::new().part("file", part);
        if let Some(language) = request.language.filter(|l| !l.is_empty()) {
            form = form.text("language", language.to_string());
        }
        if let Some(model) = &self.model {
            form = form.text("model", model.clone());
        }

        let response = self
            .client
            .post(self.url("/transcribe"))
            .multipart(form)
            .send()
            .await
            .context("uploading audio to the remote backend")?;
        read_text(response).await
    }
}

/// The wire format is fixed at 16 kHz mono; PipeWire already gives us that, so
/// this is a no-op for live capture and only does work for imported files.
fn resample_for_wire(samples: &[f32], sample_rate: u32) -> Vec<f32> {
    wav::resample(samples, sample_rate, PCM_SAMPLE_RATE)
}

async fn read_text(response: reqwest::Response) -> Result<String> {
    let status = response.status();
    let body = response.text().await.context("reading the response body")?;

    if !status.is_success() {
        let detail = body.trim();
        bail!(
            "remote backend returned {status}{}",
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {}", truncate(detail, 300))
            }
        );
    }

    Ok(parse_body(&body))
}

/// Accept a JSON object, a bare JSON string, or plain text.
fn parse_body(body: &str) -> String {
    if let Ok(parsed) = serde_json::from_str::<TranscriptionResponse>(body) {
        if let Some(text) = parsed.into_text() {
            return text.trim().to_string();
        }
    }
    if let Ok(text) = serde_json::from_str::<String>(body) {
        return text.trim().to_string();
    }
    body.trim().to_string()
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect::<String>() + "…"
}

#[async_trait]
impl Backend for RemoteBackend {
    fn info(&self) -> BackendInfo {
        BackendInfo {
            id: "remote".into(),
            model: self.model.clone(),
            local: false,
            description: format!("remote ASR at {}", self.base_url),
        }
    }

    async fn load(&self) -> Result<()> {
        if !self.warmup {
            self.ready.store(true, Ordering::SeqCst);
            return Ok(());
        }

        // Open the connection now so the first dictation is pure inference time.
        // A server without /health is fine - the handshake is the point.
        let result = tokio::time::timeout(
            self.timeout.min(Duration::from_secs(5)),
            self.client.get(self.url("/health")).send(),
        )
        .await;

        match result {
            Ok(Ok(response)) => {
                tracing::info!(
                    "remote backend reachable at {} ({})",
                    self.base_url,
                    response.status()
                );
                self.ready.store(true, Ordering::SeqCst);
                Ok(())
            }
            Ok(Err(err)) => {
                self.ready.store(false, Ordering::SeqCst);
                Err(anyhow!(err))
                    .with_context(|| format!("remote backend at {} is unreachable", self.base_url))
            }
            Err(_) => {
                self.ready.store(false, Ordering::SeqCst);
                bail!("remote backend at {} did not answer in time", self.base_url)
            }
        }
    }

    async fn transcribe(&self, request: TranscribeRequest<'_>) -> Result<Transcript> {
        let started = Instant::now();

        let text = match Wire::from_u8(self.wire.load(Ordering::Relaxed)) {
            Wire::Pcm => self.post_pcm(&request).await?.ok_or_else(|| {
                anyhow!(
                    "server rejected the raw PCM endpoint; set asr.remote.protocol = \"multipart\""
                )
            })?,
            Wire::Multipart => self.post_multipart(&request).await?,
            Wire::Unknown => match self.post_pcm(&request).await {
                Ok(Some(text)) => {
                    self.wire.store(Wire::Pcm as u8, Ordering::Relaxed);
                    text
                }
                Ok(None) => {
                    tracing::info!("remote backend has no raw-PCM route; using WAV upload");
                    self.wire.store(Wire::Multipart as u8, Ordering::Relaxed);
                    self.post_multipart(&request).await?
                }
                Err(err) => {
                    // A transport failure says nothing about which route the
                    // server supports, so the probe stays unresolved.
                    return Err(err);
                }
            },
        };

        self.ready.store(true, Ordering::SeqCst);
        Ok(Transcript {
            text,
            latency: started.elapsed(),
            backend: "remote".into(),
            model: self.model.clone(),
        })
    }

    fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn config() -> RemoteConfig {
        RemoteConfig {
            url: "http://example.invalid:8787/".into(),
            ..RemoteConfig::default()
        }
    }

    #[test]
    fn a_trailing_slash_in_the_url_does_not_double_up() {
        let backend = RemoteBackend::new(&config()).unwrap();
        assert_eq!(
            backend.url("/transcribe"),
            "http://example.invalid:8787/transcribe"
        );
    }

    #[test]
    fn the_configured_protocol_pins_the_wire_format() {
        let mut cfg = config();
        cfg.protocol = RemoteProtocol::Multipart;
        let backend = RemoteBackend::new(&cfg).unwrap();
        assert_eq!(
            Wire::from_u8(backend.wire.load(Ordering::Relaxed)),
            Wire::Multipart
        );

        cfg.protocol = RemoteProtocol::Auto;
        let backend = RemoteBackend::new(&cfg).unwrap();
        assert_eq!(
            Wire::from_u8(backend.wire.load(Ordering::Relaxed)),
            Wire::Unknown
        );
    }

    #[test]
    fn responses_are_accepted_as_json_objects_bare_strings_or_plain_text() {
        assert_eq!(parse_body(r#"{"text":"  hello  "}"#), "hello");
        assert_eq!(parse_body(r#"{"transcript":"hi"}"#), "hi");
        assert_eq!(parse_body(r#"{"result":"yo"}"#), "yo");
        assert_eq!(parse_body(r#""quoted""#), "quoted");
        assert_eq!(parse_body("plain text\n"), "plain text");
    }

    #[test]
    fn an_unrecognized_json_shape_falls_back_to_the_raw_body() {
        assert_eq!(parse_body(r#"{"nope":1}"#), r#"{"nope":1}"#);
    }

    #[test]
    fn language_and_model_become_query_parameters() {
        let mut cfg = config();
        cfg.model = Some("nvidia/parakeet-unified-en-0.6b".into());
        let backend = RemoteBackend::new(&cfg).unwrap();
        let query = backend.query(Some("en"));
        assert!(query.contains(&("language".into(), "en".into())));
        assert!(query.contains(&("model".into(), "nvidia/parakeet-unified-en-0.6b".into())));
        // An empty language is omitted rather than sent as "".
        assert!(backend.query(Some("")).iter().all(|(k, _)| k != "language"));
    }

    #[test]
    fn an_invalid_header_name_is_rejected_at_construction() {
        let mut cfg = config();
        cfg.headers.insert("bad header".into(), "value".into());
        assert!(RemoteBackend::new(&cfg).is_err());
    }

    #[test]
    fn capture_rate_audio_reaches_the_wire_unresampled() {
        let samples = vec![0.25_f32; 1000];
        assert_eq!(resample_for_wire(&samples, 16_000).len(), 1000);
        assert_eq!(resample_for_wire(&samples, 48_000).len(), 333);
    }

    #[test]
    fn error_bodies_are_truncated_rather_than_flooding_the_log() {
        let long = "x".repeat(1000);
        let truncated = truncate(&long, 300);
        assert_eq!(truncated.chars().count(), 301);
        assert!(truncated.ends_with('…'));
        assert_eq!(truncate("short", 300), "short");
    }

    #[tokio::test]
    async fn pcm_requests_work_over_a_reused_client() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (request_tx, mut request_rx) = tokio::sync::mpsc::channel(2);
        tokio::spawn(async move {
            for body in [r#"{}"#, r#"{"text":"hello"}"#] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = read_request(&mut stream).await;
                request_tx.send(request).await.unwrap();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });

        let config = RemoteConfig {
            url: format!("http://{address}"),
            protocol: RemoteProtocol::Pcm,
            model: Some("parakeet".into()),
            ..RemoteConfig::default()
        };
        let backend = RemoteBackend::new(&config).unwrap();
        backend.load().await.unwrap();
        let transcript = backend
            .transcribe(TranscribeRequest {
                samples: &[0.25; 160],
                sample_rate: 16_000,
                language: Some("en"),
                prompt: None,
            })
            .await
            .unwrap();

        let health = request_rx.recv().await.unwrap();
        let pcm = request_rx.recv().await.unwrap();
        assert!(health.starts_with("GET /health HTTP/1.1"));
        assert!(pcm.starts_with("POST /transcribe/pcm?language=en&model=parakeet HTTP/1.1"));
        assert_eq!(transcript.text, "hello");
    }

    async fn read_request(stream: &mut tokio::net::TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 1024];
        let header_end = loop {
            let count = stream.read(&mut chunk).await.unwrap();
            bytes.extend_from_slice(&chunk[..count]);
            if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let headers = String::from_utf8_lossy(&bytes[..header_end]);
        let length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length: ")
                    .map(str::to_string)
            })
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        while bytes.len() < header_end + length {
            let count = stream.read(&mut chunk).await.unwrap();
            bytes.extend_from_slice(&chunk[..count]);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }
}
