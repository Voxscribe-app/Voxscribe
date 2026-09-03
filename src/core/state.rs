//! Observable daemon state.
//!
//! One `watch` channel carries the latest snapshot (late subscribers get the
//! current value) and one `broadcast` channel carries discrete events. Nothing
//! here blocks, so state updates never stall audio or IPC.

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, watch};

use crate::core::config::RecordingMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Backend still loading; recording requests are refused.
    Starting,
    Idle,
    Recording,
    /// Long-form recording held open between segments.
    Paused,
    Processing,
    Error,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Idle => "idle",
            Self::Recording => "recording",
            Self::Paused => "paused",
            Self::Processing => "processing",
            Self::Error => "error",
        }
    }

    /// Class name consumed by the Waybar/Quickshell adapters. `idle` maps to
    /// hyprwhspr's `stopped` so existing stylesheets keep working.
    pub fn css_class(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Idle => "stopped",
            Self::Recording => "recording",
            Self::Paused => "paused",
            Self::Processing => "processing",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Snapshot {
    pub phase: Phase,
    pub mode: String,
    /// Smoothed capture level in 0.0..=1.0, 0 when not recording.
    pub level: f32,
    pub backend: String,
    pub model: Option<String>,
    /// Backend loaded and able to transcribe.
    pub ready: bool,
    pub message: String,
    pub recording_ms: u64,
    /// Long-form segments captured so far.
    pub segments: usize,
    pub last_transcript: Option<String>,
    pub last_latency_ms: Option<u64>,
    pub language: Option<String>,
    pub version: String,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            phase: Phase::Starting,
            mode: RecordingMode::Toggle.as_str().to_string(),
            level: 0.0,
            backend: String::new(),
            model: None,
            ready: false,
            message: "starting".into(),
            recording_ms: 0,
            segments: 0,
            last_transcript: None,
            last_latency_ms: None,
            language: None,
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

impl Snapshot {
    /// Human-readable tooltip, matching the shape the Quickshell service expects.
    pub fn tooltip(&self) -> String {
        let mut lines = vec![match self.phase {
            Phase::Starting => "Duskr: starting".to_string(),
            Phase::Idle => "Duskr: ready".to_string(),
            Phase::Recording => format!(
                "Duskr: recording ({:.1}s)",
                self.recording_ms as f64 / 1000.0
            ),
            Phase::Paused => format!("Duskr: paused ({} segments)", self.segments),
            Phase::Processing => "Duskr: transcribing".to_string(),
            Phase::Error => format!("Duskr: {}", self.message),
        }];
        let model = self.model.clone().unwrap_or_else(|| "-".into());
        lines.push(format!("{} · {}", self.backend, model));
        if let Some(latency) = self.last_latency_ms {
            lines.push(format!("last: {latency} ms"));
        }
        if let Some(text) = &self.last_transcript {
            let preview: String = text.chars().take(80).collect();
            if !preview.trim().is_empty() {
                lines.push(preview);
            }
        }
        lines.join("\n")
    }
}

/// Discrete notifications for IPC subscribers. Snapshot deltas are separate so a
/// slow subscriber can miss events without corrupting the visible state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    State(Snapshot),
    Level {
        level: f32,
    },
    /// Streaming/interim text, or the final transcript before injection.
    Transcript {
        text: String,
        final_: bool,
    },
    Injected {
        text: String,
    },
    Error {
        message: String,
    },
    Shutdown,
}

#[derive(Clone)]
pub struct StateHandle {
    snapshot: watch::Sender<Snapshot>,
    events: broadcast::Sender<Event>,
}

impl StateHandle {
    pub fn new(initial: Snapshot) -> Self {
        let (snapshot, _) = watch::channel(initial);
        let (events, _) = broadcast::channel(256);
        Self { snapshot, events }
    }

    pub fn get(&self) -> Snapshot {
        self.snapshot.borrow().clone()
    }

    pub fn subscribe_snapshot(&self) -> watch::Receiver<Snapshot> {
        self.snapshot.subscribe()
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// Mutate the snapshot; a `State` event is emitted only if something changed.
    pub fn update(&self, f: impl FnOnce(&mut Snapshot)) {
        let mut next = self.snapshot.borrow().clone();
        let before = next.clone();
        f(&mut next);
        if next == before {
            return;
        }
        self.snapshot.send_replace(next.clone());
        let _ = self.events.send(Event::State(next));
    }

    /// Level updates are high-frequency, so they bypass the snapshot-diff path
    /// and are published as their own event.
    pub fn set_level(&self, level: f32) {
        let level = level.clamp(0.0, 1.0);
        let changed = (self.snapshot.borrow().level - level).abs() > 0.002;
        if changed {
            self.snapshot.send_modify(|s| s.level = level);
            let _ = self.events.send(Event::Level { level });
        }
    }

    pub fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_without_change_emits_nothing() {
        let state = StateHandle::new(Snapshot::default());
        let mut events = state.subscribe_events();
        state.update(|s| s.phase = Phase::Starting);
        assert!(events.try_recv().is_err());
        state.update(|s| s.phase = Phase::Idle);
        assert!(matches!(events.try_recv(), Ok(Event::State(_))));
    }

    #[test]
    fn updates_survive_without_snapshot_subscribers() {
        let state = StateHandle::new(Snapshot::default());
        state.update(|snapshot| {
            snapshot.phase = Phase::Idle;
            snapshot.ready = true;
        });
        assert_eq!(state.get().phase, Phase::Idle);
        assert!(state.get().ready);
    }

    #[test]
    fn level_changes_below_the_threshold_are_coalesced() {
        let state = StateHandle::new(Snapshot::default());
        let mut events = state.subscribe_events();
        state.set_level(0.5);
        assert!(matches!(events.try_recv(), Ok(Event::Level { .. })));
        state.set_level(0.5005);
        assert!(events.try_recv().is_err());
        state.set_level(0.9);
        assert!(matches!(events.try_recv(), Ok(Event::Level { .. })));
    }

    #[test]
    fn tooltip_reports_phase_and_backend() {
        let snapshot = Snapshot {
            phase: Phase::Recording,
            backend: "remote".into(),
            model: Some("parakeet".into()),
            recording_ms: 2_500,
            ..Snapshot::default()
        };
        let tooltip = snapshot.tooltip();
        assert!(tooltip.starts_with("Duskr: recording (2.5s)"));
        assert!(tooltip.contains("remote · parakeet"));
    }

    #[test]
    fn idle_maps_to_the_legacy_stopped_class() {
        assert_eq!(Phase::Idle.css_class(), "stopped");
        assert_eq!(Phase::Recording.css_class(), "recording");
    }
}
