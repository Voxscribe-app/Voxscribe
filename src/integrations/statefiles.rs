//! State mirrored to files for shell integrations that cannot hold a socket
//! open - Waybar's `exec`, ad-hoc scripts, a `cat` in a terminal.
//!
//! Event-driven IPC is the primary interface; these files exist so a two-line
//! shell snippet still works. Writes are atomic so a reader never sees a
//! half-written file.

use std::path::Path;

use serde::Serialize;

use crate::core::config::Integrations;
use crate::core::paths;
use crate::core::state::Snapshot;

/// Shape consumed by the Quickshell and Waybar adapters.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct StatusFile {
    /// CSS class; `stopped`, `recording`, `processing`, `paused`, `error`.
    pub class: String,
    pub text: String,
    pub alt: String,
    pub tooltip: String,
    pub level: f32,
    pub ready: bool,
    pub backend: String,
    pub model: Option<String>,
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
}

pub fn status_file(snapshot: &Snapshot) -> StatusFile {
    StatusFile {
        class: snapshot.phase.css_class().to_string(),
        text: icon_for(snapshot).to_string(),
        alt: snapshot.phase.as_str().to_string(),
        tooltip: snapshot.tooltip(),
        level: snapshot.level,
        ready: snapshot.ready,
        backend: snapshot.backend.clone(),
        model: snapshot.model.clone(),
        mode: snapshot.mode.clone(),
        transcript: snapshot.last_transcript.clone(),
    }
}

fn icon_for(snapshot: &Snapshot) -> &'static str {
    use crate::core::state::Phase;
    match snapshot.phase {
        Phase::Recording => "󰍬",
        Phase::Processing => "󰔟",
        Phase::Paused => "󰏤",
        Phase::Error => "󰍭",
        Phase::Starting => "󰔟",
        Phase::Idle => "󰍮",
    }
}

pub struct StateWriter {
    legacy: bool,
    /// Avoids rewriting an unchanged level forty times a second.
    last_level: f32,
}

impl StateWriter {
    pub fn new(config: &Integrations) -> Self {
        Self {
            legacy: config.legacy_state_files,
            last_level: -1.0,
        }
    }

    pub fn write_snapshot(&mut self, snapshot: &Snapshot) {
        let status = status_file(snapshot);
        let Ok(json) = serde_json::to_vec_pretty(&status) else {
            return;
        };

        if let Err(err) = paths::write_atomic(&paths::state_file(), &json) {
            tracing::debug!("could not write the state file: {err}");
        }
        if self.legacy {
            let _ = paths::write_atomic(&paths::legacy_state_file(), &json);
        }
        self.write_level(snapshot.level, true);
    }

    /// Publish the capture level. Written only when it moves enough to matter.
    pub fn write_level(&mut self, level: f32, force: bool) {
        if !force && (level - self.last_level).abs() < 0.01 {
            return;
        }
        self.last_level = level;
        let body = format!("{level:.3}");
        let _ = paths::write_atomic(&paths::audio_level_file(), body.as_bytes());
        if self.legacy {
            let _ = paths::write_atomic(&paths::legacy_audio_level_file(), body.as_bytes());
        }
    }

    pub fn write_transcript_preview(&self, text: &str) {
        let _ = paths::write_atomic(&paths::transcript_preview_file(), text.as_bytes());
    }

    /// Remove everything this writer created, on shutdown.
    pub fn cleanup(&self) {
        for path in [
            paths::state_file(),
            paths::audio_level_file(),
            paths::transcript_preview_file(),
        ] {
            let _ = std::fs::remove_file(path);
        }
        if self.legacy {
            for path in [paths::legacy_state_file(), paths::legacy_audio_level_file()] {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

pub fn read_level(path: &Path) -> Option<f32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::state::Phase;

    #[test]
    fn the_status_file_uses_the_legacy_class_names() {
        let mut snapshot = Snapshot {
            phase: Phase::Idle,
            ..Snapshot::default()
        };
        assert_eq!(status_file(&snapshot).class, "stopped");
        snapshot.phase = Phase::Recording;
        assert_eq!(status_file(&snapshot).class, "recording");
    }

    #[test]
    fn the_status_file_carries_everything_a_shell_widget_needs() {
        let snapshot = Snapshot {
            phase: Phase::Recording,
            backend: "remote".into(),
            model: Some("parakeet".into()),
            level: 0.42,
            ready: true,
            ..Snapshot::default()
        };

        let status = status_file(&snapshot);
        assert_eq!(status.level, 0.42);
        assert!(status.ready);
        assert_eq!(status.backend, "remote");
        assert!(status.tooltip.contains("recording"));
        assert!(!status.text.is_empty());
    }

    #[test]
    fn levels_are_parsed_back_from_the_file_they_are_written_to() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audio_level");
        std::fs::write(&path, "0.750\n").unwrap();
        assert_eq!(read_level(&path), Some(0.75));

        std::fs::write(&path, "not a number").unwrap();
        assert_eq!(read_level(&path), None);
        assert_eq!(read_level(&dir.path().join("missing")), None);
    }

    #[test]
    fn each_phase_has_its_own_indicator() {
        let mut seen = std::collections::HashSet::new();
        for phase in [
            Phase::Idle,
            Phase::Recording,
            Phase::Processing,
            Phase::Paused,
            Phase::Error,
        ] {
            let snapshot = Snapshot {
                phase,
                ..Snapshot::default()
            };
            assert!(seen.insert(icon_for(&snapshot)), "{phase:?} reuses an icon");
        }
    }
}
