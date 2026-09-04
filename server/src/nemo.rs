use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tempfile::Builder as TempBuilder;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::{run, Transcription, SAMPLE_RATE};

const PROTOCOL_PREFIX: &str = "@@DUSKR@@";
const WORKER_SOURCE: &str = include_str!("../assets/worker.py");
const CUDA_126_INDEX: &str = "https://download.pytorch.org/whl/cu126";

pub const DEFAULT_MODEL: &str = "nvidia/parakeet-unified-en-0.6b";

#[derive(Clone, Copy, Debug, Serialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Precision {
    Auto,
    Fp32,
    Fp16,
}

impl Precision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Fp32 => "fp32",
            Self::Fp16 => "fp16",
        }
    }
}

pub struct Paths {
    pub data_dir: PathBuf,
    pub venv_dir: PathBuf,
    pub venv_python: PathBuf,
    pub worker: PathBuf,
}

impl Paths {
    pub fn new(data_dir: Option<PathBuf>) -> Result<Self> {
        let data_dir = match data_dir {
            Some(path) => path,
            None => dirs::data_dir()
                .context("XDG data directory is unavailable")?
                .join("duskr/server/nemo"),
        };
        let venv_dir = data_dir.join(".venv");
        Ok(Self {
            venv_python: venv_dir.join("bin/python"),
            worker: data_dir.join("worker.py"),
            venv_dir,
            data_dir,
        })
    }
}

fn command_succeeds(program: &str, args: &[&str]) -> bool {
    std::process::Command::new(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn find_python() -> Result<String> {
    for candidate in ["python3.12", "python3"] {
        let ok = std::process::Command::new(candidate)
            .arg("-c")
            .arg("import sys; raise SystemExit(0 if sys.version_info >= (3, 12) else 1)")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if matches!(ok, Ok(status) if status.success()) {
            return Ok(candidate.to_string());
        }
    }
    bail!("Python 3.12+ was not found; install Python 3.12 or uv")
}

/// Creates the venv and installs a CUDA 12.6 PyTorch, which still carries
/// Pascal sm_61 kernels.
pub fn setup(paths: &Paths, force: bool) -> Result<()> {
    std::fs::create_dir_all(&paths.data_dir)
        .with_context(|| format!("creating {}", paths.data_dir.display()))?;
    std::fs::write(&paths.worker, WORKER_SOURCE)
        .with_context(|| format!("writing {}", paths.worker.display()))?;

    if force && paths.venv_dir.exists() {
        std::fs::remove_dir_all(&paths.venv_dir)?;
    }

    let uv = command_succeeds("uv", &["--version"]);

    if !paths.venv_python.exists() {
        if uv {
            run(std::process::Command::new("uv")
                .args(["venv", "--python", "3.12"])
                .arg(&paths.venv_dir))?;
        } else {
            let python = find_python()?;
            run(std::process::Command::new(&python)
                .args(["-m", "venv"])
                .arg(&paths.venv_dir))?;
        }
    }

    if uv {
        run(std::process::Command::new("uv")
            .args(["pip", "install", "--python"])
            .arg(&paths.venv_python)
            .args(["torch", "torchaudio", "--index-url", CUDA_126_INDEX]))?;
        run(std::process::Command::new("uv")
            .args(["pip", "install", "--python"])
            .arg(&paths.venv_python)
            .args(["nemo-toolkit[asr]", "soundfile"]))?;
    } else {
        run(std::process::Command::new(&paths.venv_python).args([
            "-m",
            "pip",
            "install",
            "--upgrade",
            "pip",
        ]))?;
        run(std::process::Command::new(&paths.venv_python).args([
            "-m",
            "pip",
            "install",
            "torch",
            "torchaudio",
            "--index-url",
            CUDA_126_INDEX,
        ]))?;
        run(std::process::Command::new(&paths.venv_python).args([
            "-m",
            "pip",
            "install",
            "nemo-toolkit[asr]",
            "soundfile",
        ]))?;
    }

    run(std::process::Command::new(&paths.venv_python)
        .arg("-c")
        .arg(concat!(
            "import torch; ",
            "print('torch:', torch.__version__); ",
            "print('torch CUDA:', torch.version.cuda); ",
            "assert torch.cuda.is_available(), 'CUDA unavailable'; ",
            "print('GPU:', torch.cuda.get_device_name(0)); ",
            "print('capability:', torch.cuda.get_device_capability(0)); ",
            "print('arch list:', torch.cuda.get_arch_list())"
        )))?;

    println!("NeMo environment ready at {}", paths.data_dir.display());
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Ready {
    pub model: String,
    pub gpu: String,
    pub compute_capability: String,
    pub precision: String,
    pub load_seconds: f64,
    pub torch_version: String,
    pub torch_cuda: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WorkerResult {
    id: Option<u64>,
    text: String,
    audio_seconds: f64,
    inference_seconds: f64,
    realtime_x: Option<f64>,
    peak_allocated_gib: Option<f64>,
}

pub struct Worker {
    _child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    next_request_id: u64,
}

impl Worker {
    pub async fn spawn(paths: &Paths, model: &str, precision: Precision) -> Result<(Self, Ready)> {
        std::fs::create_dir_all(&paths.data_dir)?;
        std::fs::write(&paths.worker, WORKER_SOURCE)?;

        if !paths.venv_python.is_file() {
            bail!(
                "NeMo environment missing at {}; run `duskr-server setup --backend nemo` first",
                paths.venv_python.display()
            );
        }

        let mut child = Command::new(&paths.venv_python)
            .arg("-u")
            .arg(&paths.worker)
            .env("DUSKR_NEMO_MODEL", model)
            .env("DUSKR_NEMO_PRECISION", precision.as_str())
            .env("TOKENIZERS_PARALLELISM", "false")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("starting NeMo worker via {}", paths.venv_python.display()))?;

        let stdin = child.stdin.take().context("worker stdin unavailable")?;
        let stdout = child.stdout.take().context("worker stdout unavailable")?;

        let mut worker = Self {
            _child: child,
            stdin,
            stdout: BufReader::new(stdout).lines(),
            next_request_id: 1,
        };

        loop {
            let value = worker.read_protocol().await?;
            match value.get("type").and_then(Value::as_str) {
                Some("ready") => return Ok((worker, serde_json::from_value(value)?)),
                Some("error") => bail!("NeMo worker failed during startup: {}", message(&value)),
                _ => {}
            }
        }
    }

    pub async fn transcribe(&mut self, pcm: &[u8]) -> Result<Transcription> {
        let mut temp = TempBuilder::new()
            .prefix("duskr-")
            .suffix(".wav")
            .tempfile()
            .context("creating temporary WAV")?;
        temp.write_all(&wav_from_pcm(pcm))?;
        temp.flush()?;

        let id = self.next_request_id;
        self.next_request_id += 1;

        let request = json!({"cmd": "transcribe", "id": id, "path": temp.path()});
        self.stdin
            .write_all(format!("{}\n", serde_json::to_string(&request)?).as_bytes())
            .await?;
        self.stdin.flush().await?;

        loop {
            let value = self.read_protocol().await?;
            match value.get("type").and_then(Value::as_str) {
                Some("result") => {
                    let result: WorkerResult = serde_json::from_value(value)?;
                    if result.id != Some(id) {
                        continue;
                    }
                    return Ok(Transcription {
                        text: result.text,
                        audio_seconds: result.audio_seconds,
                        inference_seconds: result.inference_seconds,
                        realtime_x: result.realtime_x,
                        peak_allocated_gib: result.peak_allocated_gib,
                    });
                }
                Some("error") => {
                    let response_id = value.get("id").and_then(Value::as_u64);
                    if response_id.is_some() && response_id != Some(id) {
                        continue;
                    }
                    bail!("{}", message(&value));
                }
                _ => {}
            }
        }
    }

    async fn read_protocol(&mut self) -> Result<Value> {
        loop {
            let line = self
                .stdout
                .next_line()
                .await?
                .ok_or_else(|| anyhow!("NeMo worker exited unexpectedly"))?;
            match line.strip_prefix(PROTOCOL_PREFIX) {
                Some(payload) => {
                    return serde_json::from_str(payload)
                        .with_context(|| format!("bad worker protocol JSON: {payload}"))
                }
                None => tracing::debug!(target: "duskr_server::worker", "{line}"),
            }
        }
    }
}

fn message(value: &Value) -> &str {
    value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown worker error")
}

/// Canonical 16 kHz mono s16le WAV wrapper; the worker reads via soundfile.
fn wav_from_pcm(pcm: &[u8]) -> Vec<u8> {
    let data_len = pcm.len() as u32;
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_describes_the_payload() {
        let pcm = vec![0u8; 3_200];
        let wav = wav_from_pcm(&pcm);
        assert_eq!(wav.len(), 44 + pcm.len());
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(
            u32::from_le_bytes(wav[40..44].try_into().unwrap()),
            pcm.len() as u32
        );
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
    }
}
