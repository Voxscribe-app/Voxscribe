mod gpu;
mod nemo;
mod onnx;

use std::io::{Read, Write};
use std::net::SocketAddr;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header::AUTHORIZATION, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::{json, Value};

use nemo::Precision;
use onnx::Provider;

pub const SAMPLE_RATE: u32 = 16_000;

/// 100 ms of 16 kHz mono s16le.
const MIN_PCM_BYTES: usize = 3_200;

pub struct Transcription {
    pub text: String,
    pub audio_seconds: f64,
    pub inference_seconds: f64,
    pub realtime_x: Option<f64>,
    pub peak_allocated_gib: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Backend {
    Auto,
    Onnx,
    Nemo,
}

#[derive(Parser)]
#[command(version, about = "Remote ASR server for Duskr")]
struct Cli {
    #[command(subcommand)]
    command: ServerCommand,
}

#[derive(Subcommand)]
enum ServerCommand {
    Setup {
        #[arg(long, value_enum, default_value = "auto")]
        backend: Backend,
        #[arg(long)]
        model_dir: Option<PathBuf>,
        #[arg(long, default_value = onnx::DEFAULT_REPOSITORY)]
        repository: String,
        #[arg(long)]
        nemo_data_dir: Option<PathBuf>,
        #[arg(long)]
        force: bool,
    },
    Serve {
        #[arg(long, default_value = "127.0.0.1:8787")]
        bind: SocketAddr,
        #[arg(long, env = "DUSKR_SERVER_TOKEN")]
        token: Option<String>,
        #[arg(long, conflicts_with = "token")]
        token_file: Option<PathBuf>,
        #[arg(long)]
        insecure_no_auth: bool,
        #[arg(long, value_enum, default_value = "auto")]
        backend: Backend,
        #[arg(long)]
        model_dir: Option<PathBuf>,
        #[arg(long, value_enum, default_value = "cuda")]
        provider: Provider,
        #[arg(long)]
        nemo_data_dir: Option<PathBuf>,
        #[arg(long, default_value = nemo::DEFAULT_MODEL)]
        nemo_model: String,
        #[arg(long, value_enum, default_value = "auto")]
        precision: Precision,
    },
    GenerateToken {
        #[arg(long)]
        output: PathBuf,
    },
    InstallService {
        #[arg(long, default_value = "0.0.0.0:8787")]
        bind: SocketAddr,
        #[arg(long)]
        token_file: PathBuf,
        #[arg(long, value_enum, default_value = "auto")]
        backend: Backend,
        #[arg(long)]
        model_dir: Option<PathBuf>,
        #[arg(long, value_enum, default_value = "cuda")]
        provider: Provider,
        #[arg(long)]
        nemo_data_dir: Option<PathBuf>,
        #[arg(long, default_value = nemo::DEFAULT_MODEL)]
        nemo_model: String,
        #[arg(long, value_enum, default_value = "auto")]
        precision: Precision,
    },
    /// Detected GPU and what `--backend auto` would pick.
    Detect,
    Paths,
}

/// No pre-Volta cuDNN kernels in ONNX Runtime, so older cards go to NeMo and
/// its CUDA 12.6 PyTorch, which still ships Pascal SASS.
fn resolve_backend(requested: Backend, provider: Provider) -> Backend {
    if requested != Backend::Auto {
        return requested;
    }
    if provider == Provider::Cpu {
        return Backend::Onnx;
    }
    match gpu::detect() {
        Some(info) if info.supports_onnx_cuda() => {
            tracing::info!(
                gpu = %info.name,
                compute_capability = %info.capability_string(),
                "GPU supports the ONNX Runtime CUDA provider"
            );
            Backend::Onnx
        }
        Some(info) => {
            tracing::info!(
                gpu = %info.name,
                compute_capability = %info.capability_string(),
                "GPU is older than compute {}.{}; selecting the NeMo backend",
                gpu::MIN_ONNX_CUDA_CC.0,
                gpu::MIN_ONNX_CUDA_CC.1
            );
            Backend::Nemo
        }
        None => {
            tracing::warn!("nvidia-smi reported no GPU; defaulting to the ONNX backend");
            Backend::Onnx
        }
    }
}

#[derive(Clone)]
enum Runtime {
    Onnx(Arc<std::sync::Mutex<onnx::Engine>>),
    Nemo(Arc<tokio::sync::Mutex<nemo::Worker>>),
}

impl Runtime {
    async fn transcribe(&self, pcm: Bytes) -> Result<Transcription> {
        match self {
            Self::Onnx(engine) => {
                let engine = Arc::clone(engine);
                tokio::task::spawn_blocking(move || {
                    engine
                        .lock()
                        .map_err(|_| anyhow::anyhow!("model lock poisoned"))?
                        .transcribe(&pcm)
                })
                .await?
            }
            Self::Nemo(worker) => worker.lock().await.transcribe(&pcm).await,
        }
    }
}

#[derive(Clone, Serialize)]
struct ServerStatus {
    ready: bool,
    backend: &'static str,
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<Provider>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gpu: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    compute_capability: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    precision: Option<String>,
    load_seconds: f64,
}

#[derive(Clone)]
struct AppState {
    runtime: Runtime,
    token: Option<String>,
    status: ServerStatus,
}

#[derive(Debug)]
struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

fn authorize(headers: &HeaderMap, token: &Option<String>) -> Result<(), ApiError> {
    let Some(token) = token else {
        return Ok(());
    };
    let expected = format!("Bearer {token}");
    let supplied = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    if supplied == Some(expected.as_str()) {
        Ok(())
    } else {
        Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "missing or invalid bearer token".into(),
        ))
    }
}

async fn health() -> Json<Value> {
    Json(json!({"ok": true, "service": "duskr-server"}))
}

async fn status_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ServerStatus>, ApiError> {
    authorize(&headers, &state.token)?;
    Ok(Json(state.status))
}

async fn transcribe(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers, &state.token)?;
    if body.len() < MIN_PCM_BYTES || body.len() % 2 != 0 {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "PCM must contain at least 100 ms of 16 kHz mono s16le audio".into(),
        ));
    }
    let total_started = Instant::now();
    let result = state.runtime.transcribe(body).await.map_err(|error| {
        tracing::error!("transcription failed: {error:#}");
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("transcription failed: {error:#}"),
        )
    })?;
    tracing::info!(
        audio_seconds = result.audio_seconds,
        inference_ms = result.inference_seconds * 1000.0,
        total_ms = total_started.elapsed().as_secs_f64() * 1000.0,
        "transcription complete"
    );
    let mut payload = json!({
        "text": result.text,
        "audio_seconds": result.audio_seconds,
        "inference_seconds": result.inference_seconds,
    });
    if let Some(realtime_x) = result.realtime_x {
        payload["realtime_x"] = json!(realtime_x);
    }
    if let Some(peak) = result.peak_allocated_gib {
        payload["peak_allocated_gib"] = json!(peak);
    }
    Ok(Json(payload))
}

fn load_token(mut token: Option<String>, token_file: Option<&Path>) -> Result<Option<String>> {
    if let Some(path) = token_file {
        token = Some(
            std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?
                .trim()
                .to_string(),
        );
    }
    Ok(token.filter(|value| !value.is_empty()))
}

fn generate_token(path: &Path) -> Result<()> {
    let mut random = std::fs::File::open("/dev/urandom")?;
    let mut bytes = [0u8; 32];
    random.read_exact(&mut bytes)?;
    let token = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut output = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    writeln!(output, "{token}")?;
    println!("token written to {}", path.display());
    Ok(())
}

pub fn run(command: &mut Command) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("running {command:?}"))?;
    if !status.success() {
        bail!("command failed with {status}: {command:?}");
    }
    Ok(())
}

fn backend_flag(backend: Backend) -> &'static str {
    match backend {
        Backend::Auto => "auto",
        Backend::Onnx => "onnx",
        Backend::Nemo => "nemo",
    }
}

#[allow(clippy::too_many_arguments)]
fn install_service(
    bind: SocketAddr,
    token_file: &Path,
    backend: Backend,
    model_dir: &Path,
    provider: Provider,
    nemo_paths: &nemo::Paths,
    nemo_model: &str,
    precision: Precision,
) -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        bail!("install-service must run as root");
    }
    for path in [token_file, model_dir, nemo_paths.data_dir.as_path()] {
        if path
            .as_os_str()
            .to_string_lossy()
            .chars()
            .any(char::is_whitespace)
        {
            bail!(
                "service paths cannot contain whitespace: {}",
                path.display()
            );
        }
    }
    if nemo_model.chars().any(char::is_whitespace) {
        bail!("NeMo model name cannot contain whitespace");
    }
    let executable = std::env::current_exe()?;
    // The venv path comes from HOME, which systemd does not set.
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/root"));
    let unit = format!(
        "[Unit]\nDescription=Duskr remote ASR server\nAfter=network-online.target\nWants=network-online.target\n\n\
         [Service]\nType=simple\nEnvironment=HOME={home}\n\
         ExecStart={executable} serve --bind {bind} --token-file {token_file} --backend {backend} \
         --model-dir {model_dir} --provider {provider} --nemo-data-dir {nemo_data_dir} \
         --nemo-model {nemo_model} --precision {precision}\n\
         Restart=on-failure\nRestartSec=3\nNoNewPrivileges=yes\nPrivateTmp=yes\n\n\
         [Install]\nWantedBy=multi-user.target\n",
        home = home.display(),
        executable = executable.display(),
        bind = bind,
        token_file = token_file.display(),
        backend = backend_flag(backend),
        model_dir = model_dir.display(),
        provider = provider.as_str(),
        nemo_data_dir = nemo_paths.data_dir.display(),
        nemo_model = nemo_model,
        precision = precision.as_str(),
    );
    let path = Path::new("/etc/systemd/system/duskr-server.service");
    std::fs::write(path, unit)?;
    run(Command::new("systemctl").arg("daemon-reload"))?;
    println!("service installed at {}", path.display());
    Ok(())
}

/// One short inference, confirming the process holds GPU memory - a CPU
/// fallback should surface at startup, not as mysterious latency.
fn verify_onnx_cuda(engine: &mut onnx::Engine) -> Result<()> {
    let half_second = vec![0u8; SAMPLE_RATE as usize]; // 8000 samples * 2 bytes
    engine
        .transcribe(&half_second)
        .context("CUDA warmup inference failed")?;
    match gpu::process_uses_gpu(std::process::id()) {
        Some(true) => {
            tracing::info!("CUDA warmup confirmed: the model holds GPU memory");
            Ok(())
        }
        Some(false) => bail!(
            "--provider cuda was requested but the process holds no GPU memory after warmup; \
             the ONNX Runtime CUDA provider libraries are most likely not next to the binary"
        ),
        None => {
            tracing::warn!("nvidia-smi unavailable; skipped the CUDA warmup verification");
            Ok(())
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "duskr_server=info".into()),
        )
        .init();

    match Cli::parse().command {
        ServerCommand::Detect => {
            match gpu::detect() {
                Some(info) => println!(
                    "gpu={}\ncompute_capability={}\nonnx_cuda_supported={}",
                    info.name,
                    info.capability_string(),
                    info.supports_onnx_cuda()
                ),
                None => println!("gpu=none"),
            }
            println!(
                "auto_backend={}",
                backend_flag(resolve_backend(Backend::Auto, Provider::Cuda))
            );
        }
        ServerCommand::Paths => {
            println!("onnx_model={}", onnx::default_model_dir()?.display());
            let nemo_paths = nemo::Paths::new(None)?;
            println!("nemo_data={}", nemo_paths.data_dir.display());
            println!("nemo_python={}", nemo_paths.venv_python.display());
            println!("nemo_worker={}", nemo_paths.worker.display());
        }
        ServerCommand::GenerateToken { output } => generate_token(&output)?,
        ServerCommand::Setup {
            backend,
            model_dir,
            repository,
            nemo_data_dir,
            force,
        } => match resolve_backend(backend, Provider::Cuda) {
            Backend::Nemo => nemo::setup(&nemo::Paths::new(nemo_data_dir)?, force)?,
            _ => {
                onnx::setup(
                    &model_dir.unwrap_or(onnx::default_model_dir()?),
                    &repository,
                    force,
                )
                .await?
            }
        },
        ServerCommand::InstallService {
            bind,
            token_file,
            backend,
            model_dir,
            provider,
            nemo_data_dir,
            nemo_model,
            precision,
        } => install_service(
            bind,
            &token_file,
            backend,
            &model_dir.unwrap_or(onnx::default_model_dir()?),
            provider,
            &nemo::Paths::new(nemo_data_dir)?,
            &nemo_model,
            precision,
        )?,
        ServerCommand::Serve {
            bind,
            token,
            token_file,
            insecure_no_auth,
            backend,
            model_dir,
            provider,
            nemo_data_dir,
            nemo_model,
            precision,
        } => {
            let token = load_token(token, token_file.as_deref())?;
            if token.is_none() && !insecure_no_auth {
                bail!(
                    "authentication is required; set DUSKR_SERVER_TOKEN, use --token-file, or pass --insecure-no-auth"
                );
            }

            let started = Instant::now();
            let (runtime, status) = match resolve_backend(backend, provider) {
                Backend::Nemo => {
                    let paths = nemo::Paths::new(nemo_data_dir)?;
                    let (worker, ready) =
                        nemo::Worker::spawn(&paths, &nemo_model, precision).await?;
                    tracing::info!(
                        gpu = %ready.gpu,
                        compute_capability = %ready.compute_capability,
                        precision = %ready.precision,
                        torch = %ready.torch_version,
                        load_seconds = ready.load_seconds,
                        "NeMo worker ready"
                    );
                    let status = ServerStatus {
                        ready: true,
                        backend: "nemo",
                        model: ready.model,
                        provider: None,
                        gpu: Some(ready.gpu),
                        compute_capability: Some(ready.compute_capability),
                        precision: Some(ready.precision),
                        load_seconds: ready.load_seconds,
                    };
                    (
                        Runtime::Nemo(Arc::new(tokio::sync::Mutex::new(worker))),
                        status,
                    )
                }
                _ => {
                    let model_dir = model_dir.unwrap_or(onnx::default_model_dir()?);
                    let mut engine = onnx::Engine::load(&model_dir, provider)?;
                    if provider == Provider::Cuda {
                        verify_onnx_cuda(&mut engine)?;
                    }
                    let detected = gpu::detect();
                    let status = ServerStatus {
                        ready: true,
                        backend: "onnx",
                        model: onnx::MODEL_NAME.to_string(),
                        provider: Some(provider),
                        gpu: detected.as_ref().map(|info| info.name.clone()),
                        compute_capability: detected.as_ref().map(|info| info.capability_string()),
                        precision: None,
                        load_seconds: started.elapsed().as_secs_f64(),
                    };
                    tracing::info!(
                        model = onnx::MODEL_NAME,
                        provider = provider.as_str(),
                        load_seconds = status.load_seconds,
                        "ONNX model ready"
                    );
                    (
                        Runtime::Onnx(Arc::new(std::sync::Mutex::new(engine))),
                        status,
                    )
                }
            };

            let state = AppState {
                runtime,
                token,
                status,
            };
            let app = Router::new()
                .route("/health", get(health))
                .route("/v1/status", get(status_handler))
                .route("/transcribe/pcm", post(transcribe))
                .route("/v1/transcribe/pcm", post(transcribe))
                .layer(DefaultBodyLimit::max(128 * 1024 * 1024))
                .with_state(state);
            let listener = tokio::net::TcpListener::bind(bind).await?;
            tracing::info!(%bind, "listening");
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn bearer_authentication_is_required_when_configured() {
        let token = Some("secret".to_string());
        assert!(authorize(&HeaderMap::new(), &token).is_err());
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer secret"));
        assert!(authorize(&headers, &token).is_ok());
    }

    #[test]
    fn explicit_backends_are_never_overridden() {
        assert_eq!(
            resolve_backend(Backend::Onnx, Provider::Cuda),
            Backend::Onnx
        );
        assert_eq!(resolve_backend(Backend::Nemo, Provider::Cpu), Backend::Nemo);
    }

    #[test]
    fn cpu_never_auto_selects_nemo() {
        assert_eq!(resolve_backend(Backend::Auto, Provider::Cpu), Backend::Onnx);
    }
}
