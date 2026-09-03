//! XDG-derived locations for every file Duskr owns.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const APP: &str = "duskr";

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

pub fn config_dir() -> PathBuf {
    env_path("XDG_CONFIG_HOME")
        .unwrap_or_else(|| home().join(".config"))
        .join(APP)
}

pub fn data_dir() -> PathBuf {
    env_path("XDG_DATA_HOME")
        .unwrap_or_else(|| home().join(".local/share"))
        .join(APP)
}

pub fn cache_dir() -> PathBuf {
    env_path("XDG_CACHE_HOME")
        .unwrap_or_else(|| home().join(".cache"))
        .join(APP)
}

/// Private per-user runtime directory. Created with mode 0700 on first use.
pub fn runtime_dir() -> PathBuf {
    match env_path("XDG_RUNTIME_DIR") {
        Some(dir) => dir.join(APP),
        None => std::env::temp_dir().join(format!("{APP}-{}", unsafe { libc::getuid() })),
    }
}

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

pub fn config_file() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn socket_path() -> PathBuf {
    runtime_dir().join("duskr.sock")
}

pub fn pid_file() -> PathBuf {
    runtime_dir().join("duskr.pid")
}

pub fn state_file() -> PathBuf {
    runtime_dir().join("state.json")
}

pub fn audio_level_file() -> PathBuf {
    runtime_dir().join("audio_level")
}

pub fn transcript_preview_file() -> PathBuf {
    runtime_dir().join("transcript_preview")
}

/// Mirror of [`state_file`]/[`audio_level_file`] under the config directory, kept
/// for shell integrations that poll a stable path (Waybar, ad-hoc scripts).
pub fn legacy_state_file() -> PathBuf {
    config_dir().join("recording_status")
}

pub fn legacy_audio_level_file() -> PathBuf {
    config_dir().join("audio_level")
}

pub fn default_models_dir() -> PathBuf {
    data_dir().join("models")
}

pub fn history_file() -> PathBuf {
    data_dir().join("history.jsonl")
}

pub fn log_dir() -> PathBuf {
    cache_dir().join("logs")
}

pub fn ensure_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)
}

/// Create `path` if missing and force mode 0700; sockets and transcripts live here.
pub fn ensure_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    let mut perms = fs::metadata(path)?.permissions();
    if std::os::unix::fs::PermissionsExt::mode(&perms) & 0o777 != 0o700 {
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o700);
        fs::set_permissions(path, perms)?;
    }
    Ok(())
}

/// Write via a sibling temp file + rename so readers never observe a partial file.
pub fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)
}

pub fn hyprwhspr_config_dir() -> PathBuf {
    env_path("XDG_CONFIG_HOME")
        .unwrap_or_else(|| home().join(".config"))
        .join("hyprwhspr")
}

pub fn hyprwhspr_data_dir() -> PathBuf {
    env_path("XDG_DATA_HOME")
        .unwrap_or_else(|| home().join(".local/share"))
        .join("hyprwhspr")
}

pub fn pywhispercpp_models_dir() -> PathBuf {
    env_path("XDG_DATA_HOME")
        .unwrap_or_else(|| home().join(".local/share"))
        .join("pywhispercpp/models")
}

pub fn quickshell_config_dir() -> PathBuf {
    env_path("XDG_CONFIG_HOME")
        .unwrap_or_else(|| home().join(".config"))
        .join("quickshell")
}
