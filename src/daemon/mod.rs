pub mod session;
pub mod transcribe;

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::sync::{broadcast, mpsc};

use crate::asr::{self, Backend};
use crate::audio::capture::{AudioEvent, Capture};
use crate::audio::ducking::Ducker;
use crate::audio::feedback::{Feedback, Sound};
use crate::audio::level;
use crate::audio::vad::{Vad, VadState};
use crate::core::config::{Config, RecordingMode};
use crate::core::paths;
use crate::core::state::{Event, Phase, Snapshot, StateHandle};
use crate::input::hotkeys::{self, HotkeyEvent, HotkeyListener};
use crate::input::inject::Injector;
use crate::integrations::notify::{Notifier, Urgency};
use crate::integrations::osd::Osd;
use crate::integrations::statefiles::StateWriter;
use crate::ipc::server::{Command, Server};
use crate::ipc::{Request, Response};

use session::{Action, AutoModeState, SessionPhase, SessionRules};

const LEVEL_INTERVAL: Duration = Duration::from_millis(50);
const MUTE_GRACE: Duration = Duration::from_millis(600);
const MUTE_TIMEOUT: Duration = Duration::from_millis(1_200);

pub struct Daemon {
    config: Arc<Config>,
    state: StateHandle,
    capture: Capture,
    feedback: Feedback,
    ducker: Option<Ducker>,
    backend: Arc<dyn Backend>,
    injector: Option<Arc<Injector>>,
    notifier: Arc<Notifier>,
    writer: StateWriter,
    osd: Option<Osd>,
    osd_suppressed: bool,
    jobs: mpsc::UnboundedSender<transcribe::Job>,

    phase: SessionPhase,
    auto: AutoModeState,
    press_at: Option<Instant>,
    started_at: Option<Instant>,
    language: Option<String>,
    segments: Vec<Vec<f32>>,
    vad: Option<Vad>,
    silence_since: Option<Instant>,
    last_level_sample: Instant,
    next_audio_retry: Option<Instant>,
    hotkeys: Option<HotkeyListener>,
}

pub async fn run() -> Result<()> {
    let config = Arc::new(Config::load_or_default());
    let state = StateHandle::new(Snapshot {
        mode: config.general.recording_mode.as_str().to_string(),
        language: config.general.language.clone(),
        backend: asr::canonical_backend_id(&config.asr.backend).to_string(),
        ..Snapshot::default()
    });

    paths::ensure_private_dir(&paths::runtime_dir()).context("preparing the runtime directory")?;

    let (audio_tx, mut audio_rx) = mpsc::unbounded_channel::<AudioEvent>();
    let (hotkey_tx, mut hotkey_rx) = mpsc::unbounded_channel::<HotkeyEvent>();
    let (command_tx, mut command_rx) = mpsc::channel::<Command>(32);
    let (job_tx, job_rx) = mpsc::unbounded_channel::<transcribe::Job>();
    let (shutdown_tx, shutdown_rx) = broadcast::channel::<()>(4);
    let (suspend_tx, mut suspend_rx) = mpsc::unbounded_channel::<bool>();

    let capture =
        Capture::start(&config.audio, audio_tx.clone()).context("starting PipeWire capture")?;
    let feedback = Feedback::new(&config.audio);

    let ducker = if config.audio.ducking {
        match Ducker::start() {
            Ok(ducker) => Some(ducker),
            Err(err) => {
                tracing::warn!("audio ducking disabled: {err:#}");
                None
            }
        }
    } else {
        None
    };

    let injector = match Injector::new(&config) {
        Ok(injector) => Some(Arc::new(injector)),
        Err(err) => {
            tracing::error!("text injection unavailable: {err:#}");
            state.update(|snapshot| {
                snapshot.phase = Phase::Error;
                snapshot.message = format!("input unavailable: {err}");
            });
            None
        }
    };

    let notifier =
        Arc::new(Notifier::new(config.integrations.notifications || config.integrations.osd).await);

    let backend: Arc<dyn Backend> = asr::build(&config)
        .context("creating the ASR backend")?
        .into();

    {
        let backend = Arc::clone(&backend);
        let state = state.clone();
        let notifier = Arc::clone(&notifier);
        tokio::spawn(async move {
            match backend.load().await {
                Ok(()) => {
                    let info = backend.info();
                    state.update(|snapshot| {
                        snapshot.ready = true;
                        snapshot.backend = info.id.clone();
                        snapshot.model = info.model.clone();
                        if snapshot.phase == Phase::Starting {
                            snapshot.phase = Phase::Idle;
                            snapshot.message = "ready".into();
                        }
                    })
                }
                Err(err) => {
                    let message = format!("{err:#}");
                    tracing::error!("backend failed to load: {message}");
                    state.update(|snapshot| {
                        snapshot.phase = Phase::Error;
                        snapshot.ready = false;
                        snapshot.message = message.clone();
                    });
                    notifier.error(&message).await;
                }
            }
        });
    }

    tokio::spawn(
        transcribe::Worker {
            backend: Arc::clone(&backend),
            injector: injector.clone(),
            state: state.clone(),
        }
        .run(job_rx),
    );

    let hotkeys = start_hotkeys(&config, hotkey_tx, &state);

    let server = Server::bind(&paths::socket_path())?;
    tracing::info!("listening on {}", server.path().display());
    tokio::spawn(server.run(command_tx, state.clone(), shutdown_rx));

    {
        let suspend_tx = suspend_tx.clone();
        tokio::spawn(async move {
            if let Err(err) = crate::integrations::notify::watch_suspend(move |sleeping| {
                let _ = suspend_tx.send(sleeping);
            })
            .await
            {
                tracing::debug!("suspend watcher unavailable: {err}");
            }
        });
    }

    let mut daemon = Daemon {
        writer: StateWriter::new(&config.integrations),
        osd: Osd::start(&config.osd),
        osd_suppressed: false,
        config,
        state,
        capture,
        feedback,
        ducker,
        backend,
        injector,
        notifier,
        jobs: job_tx,
        phase: SessionPhase::Idle,
        auto: AutoModeState::default(),
        press_at: None,
        started_at: None,
        language: None,
        segments: Vec::new(),
        vad: None,
        silence_since: None,
        last_level_sample: Instant::now(),
        next_audio_retry: None,
        hotkeys,
    };

    daemon.write_pid();
    let mut snapshots = daemon.state.subscribe_snapshot();
    let mut ticker = tokio::time::interval(LEVEL_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;

    loop {
        tokio::select! {
            Some(event) = hotkey_rx.recv() => daemon.on_hotkey(event).await,
            Some(event) = audio_rx.recv() => daemon.on_audio(event).await,
            Some(command) = command_rx.recv() => {
                if daemon.on_command(command).await {
                    break;
                }
            }
            Some(sleeping) = suspend_rx.recv() => daemon.on_suspend(sleeping).await,
            _ = ticker.tick() => daemon.on_tick().await,
            Ok(()) = snapshots.changed() => {
                let snapshot = snapshots.borrow_and_update().clone();
                daemon.writer.write_snapshot(&snapshot);
                daemon.run_state_hook(&snapshot);
                daemon.update_osd(&snapshot);
            }
            _ = tokio::signal::ctrl_c() => break,
            _ = sigterm.recv() => break,
            _ = sighup.recv() => {
                if let Err(err) = daemon.reload().await {
                    tracing::warn!("reload failed: {err:#}");
                }
            }
        }
    }

    tracing::info!("shutting down");
    daemon.shutdown(&shutdown_tx).await;
    Ok(())
}

fn start_hotkeys(
    config: &Config,
    hotkey_tx: mpsc::UnboundedSender<HotkeyEvent>,
    state: &StateHandle,
) -> Option<HotkeyListener> {
    let (specs, problems) = hotkeys::resolve_bindings(&config.shortcuts);
    for problem in &problems {
        tracing::warn!("{problem}");
    }
    if specs.is_empty() {
        tracing::warn!("no shortcuts bound; control Duskr through the CLI or IPC");
        return None;
    }
    match HotkeyListener::start(&config.shortcuts, specs, hotkey_tx) {
        Ok(listener) => Some(listener),
        Err(err) => {
            tracing::error!("shortcuts unavailable: {err:#}");
            state.update(|snapshot| {
                snapshot.message = format!("shortcuts unavailable: {err}");
            });
            None
        }
    }
}

impl Daemon {
    fn rules(&self) -> SessionRules {
        SessionRules {
            mode: self.config.general.recording_mode,
            tap_threshold: Duration::from_millis(self.config.general.tap_threshold_ms),
            secondary_language: self.config.shortcuts.secondary_language.clone(),
        }
    }

    fn write_pid(&self) {
        let _ = paths::write_atomic(
            &paths::pid_file(),
            std::process::id().to_string().as_bytes(),
        );
    }

    async fn on_hotkey(&mut self, event: HotkeyEvent) {
        let rules = self.rules();
        let action = match event {
            HotkeyEvent::Pressed(binding) => {
                self.press_at = Some(Instant::now());
                session::on_press(&rules, self.phase, binding, &mut self.auto)
            }
            HotkeyEvent::Released(binding) => {
                let held = self
                    .press_at
                    .take()
                    .map(|at| at.elapsed())
                    .unwrap_or_default();
                session::on_release(&rules, self.phase, binding, held, &mut self.auto)
            }
        };
        self.apply(action).await;
    }

    async fn apply(&mut self, action: Action) {
        match action {
            Action::Nothing => {}
            Action::Start { language } => self.start(language).await,
            Action::Stop => self.stop(false).await,
            Action::Cancel => self.cancel().await,
            Action::Submit => self.stop(true).await,
            Action::Pause => self.pause().await,
            Action::Resume => self.resume().await,
        }
    }

    async fn start(&mut self, language: Option<String>) {
        if self.phase != SessionPhase::Idle {
            return;
        }
        if !self.backend.is_ready() {
            let message = "backend is still loading";
            tracing::info!("{message}; ignoring the start request");
            self.notifier
                .notify("Duskr", message, Urgency::Normal)
                .await;
            return;
        }
        if self.injector.is_none() {
            self.notifier
                .error("no virtual keyboard: check access to /dev/uinput")
                .await;
            return;
        }

        self.segments.clear();
        self.language = language.or_else(|| self.config.general.language.clone());
        self.capture.arm();

        self.vad = self.make_vad();
        self.silence_since = None;
        self.started_at = Some(Instant::now());
        self.next_audio_retry = None;
        self.phase = SessionPhase::Recording;

        let mode = self.config.general.recording_mode;
        self.state.update(|snapshot| {
            snapshot.phase = Phase::Recording;
            snapshot.mode = mode.as_str().to_string();
            snapshot.message = "recording".into();
            snapshot.recording_ms = 0;
            snapshot.segments = 0;
        });

        self.feedback.play(Sound::Start);
        if self.config.integrations.osd {
            self.notifier
                .notify("Duskr", "Recording", Urgency::Low)
                .await;
        }
        if let Some(ducker) = &self.ducker {
            ducker.duck(self.config.audio.ducking_percent);
        }
        tracing::info!("recording started ({})", mode.as_str());
    }

    fn make_vad(&self) -> Option<Vad> {
        let mode = self.config.general.recording_mode;
        let wants_vad =
            mode == RecordingMode::Continuous || self.config.audio.silence_timeout > 0.0;
        if !wants_vad {
            return None;
        }
        let silence = if mode == RecordingMode::Continuous {
            Duration::from_secs_f32(self.config.audio.continuous_silence_seconds.max(0.2))
        } else {
            Duration::from_secs_f32(self.config.audio.silence_timeout.max(0.2))
        };
        Some(Vad::new(
            self.config.audio.silence_threshold,
            silence,
            Duration::from_millis(200),
        ))
    }

    async fn stop(&mut self, submit_segments: bool) {
        if self.phase == SessionPhase::Idle {
            return;
        }

        let tail = self.capture.take();
        self.capture.disarm();
        self.phase = SessionPhase::Idle;
        self.started_at = None;
        self.next_audio_retry = None;
        self.vad = None;
        self.silence_since = None;
        self.auto = AutoModeState::default();

        if let Some(ducker) = &self.ducker {
            ducker.restore();
        }
        self.feedback.play(Sound::Stop);
        if self.config.integrations.osd {
            self.notifier
                .notify("Duskr", "Transcribing", Urgency::Low)
                .await;
        }

        let mut samples = if submit_segments || !self.segments.is_empty() {
            let mut all: Vec<f32> = self.segments.drain(..).flatten().collect();
            all.extend_from_slice(&tail);
            all
        } else {
            tail
        };
        if samples.len() < (self.capture.sample_rate() as f32 * 0.1) as usize {
            samples.clear();
        }

        if samples.is_empty() {
            tracing::info!("nothing recorded");
            self.state.update(|snapshot| {
                snapshot.phase = Phase::Idle;
                snapshot.message = "ready".into();
                snapshot.recording_ms = 0;
                snapshot.segments = 0;
            });
            self.state.set_level(0.0);
            self.writer.write_level(0.0, true);
            return;
        }

        self.state.update(|snapshot| {
            snapshot.phase = Phase::Processing;
            snapshot.message = "transcribing".into();
            snapshot.segments = 0;
        });
        self.state.set_level(0.0);
        self.writer.write_level(0.0, true);

        self.queue(samples, false);
    }

    async fn cancel(&mut self) {
        if self.phase == SessionPhase::Idle {
            return;
        }
        let _ = self.capture.take();
        self.capture.disarm();
        self.segments.clear();
        self.phase = SessionPhase::Idle;
        self.started_at = None;
        self.next_audio_retry = None;
        self.vad = None;
        self.auto = AutoModeState::default();

        if let Some(ducker) = &self.ducker {
            ducker.restore();
        }
        self.state.update(|snapshot| {
            snapshot.phase = Phase::Idle;
            snapshot.message = "cancelled".into();
            snapshot.recording_ms = 0;
            snapshot.segments = 0;
        });
        self.state.set_level(0.0);
        self.writer.write_level(0.0, true);
        tracing::info!("recording cancelled");
        if self.config.integrations.osd {
            self.notifier.clear().await;
        }
    }

    async fn pause(&mut self) {
        if self.phase != SessionPhase::Recording {
            return;
        }
        let segment = self.capture.take();
        if !segment.is_empty() {
            self.segments.push(segment);
        }
        self.capture.disarm();
        self.phase = SessionPhase::Paused;

        let segments = self.segments.len();
        self.state.update(|snapshot| {
            snapshot.phase = Phase::Paused;
            snapshot.message = format!("paused ({segments} segments)");
            snapshot.segments = segments;
        });
        self.state.set_level(0.0);
        if let Some(ducker) = &self.ducker {
            ducker.restore();
        }
    }

    async fn resume(&mut self) {
        if self.phase != SessionPhase::Paused {
            return;
        }
        self.capture.arm();
        self.phase = SessionPhase::Recording;
        self.started_at = Some(Instant::now());
        self.next_audio_retry = None;
        self.state.update(|snapshot| {
            snapshot.phase = Phase::Recording;
            snapshot.message = "recording".into();
        });
        if let Some(ducker) = &self.ducker {
            ducker.duck(self.config.audio.ducking_percent);
        }
    }

    fn queue(&self, samples: Vec<f32>, keep_recording: bool) {
        let _ = self.jobs.send(transcribe::Job {
            samples,
            sample_rate: self.capture.sample_rate(),
            language: self.language.clone(),
            config: Arc::clone(&self.config),
            keep_recording,
        });
    }

    async fn on_tick(&mut self) {
        let level = self.capture.level();
        if self.phase == SessionPhase::Recording {
            self.state.set_level(level);
            self.writer.write_level(level, false);
        }

        let elapsed = self.last_level_sample.elapsed();
        self.last_level_sample = Instant::now();

        if self.phase != SessionPhase::Recording {
            return;
        }

        if let Some(started) = self.started_at {
            let recording_ms = started.elapsed().as_millis() as u64;
            self.state
                .update(|snapshot| snapshot.recording_ms = recording_ms);

            let limit = Duration::from_secs(self.config.audio.max_recording_seconds as u64);
            if !limit.is_zero() && started.elapsed() >= limit {
                tracing::info!("recording hit the {}s limit", limit.as_secs());
                self.stop(false).await;
                return;
            }

            if started.elapsed() >= Duration::from_secs(1)
                && self.capture.frames() == 0
                && self
                    .next_audio_retry
                    .is_none_or(|retry| Instant::now() >= retry)
            {
                tracing::warn!("capture is not producing audio; reconnecting");
                self.capture.reconnect();
                self.next_audio_retry = Some(Instant::now() + Duration::from_secs(2));
            }

            if self.config.general.recording_mode == RecordingMode::LongForm {
                let segment_limit =
                    Duration::from_secs(self.config.audio.long_form_segment_seconds as u64);
                if !segment_limit.is_zero() && started.elapsed() >= segment_limit {
                    let segment = self.capture.drain();
                    if !segment.is_empty() {
                        self.segments.push(segment);
                        self.started_at = Some(Instant::now());
                        let count = self.segments.len();
                        self.state.update(|snapshot| snapshot.segments = count);
                    }
                }

                let samples =
                    self.capture.sample_count() + self.segments.iter().map(Vec::len).sum::<usize>();
                let bytes = samples.saturating_mul(std::mem::size_of::<f32>());
                let byte_limit = self.config.audio.long_form_limit_mb as usize * 1024 * 1024;
                if byte_limit > 0 && bytes >= byte_limit {
                    self.notifier
                        .notify("Duskr", "Long-form size limit reached", Urgency::Normal)
                        .await;
                    self.stop(true).await;
                    return;
                }
            }
        }

        if self.check_muted().await {
            return;
        }
        self.check_vad(elapsed).await;
    }

    async fn check_muted(&mut self) -> bool {
        if !self.config.audio.mute_detection {
            return false;
        }
        let Some(started) = self.started_at else {
            return false;
        };
        if started.elapsed() < MUTE_GRACE {
            return false;
        }

        if level::is_digital_silence(self.capture.raw_level()) {
            let since = *self.silence_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= MUTE_TIMEOUT {
                tracing::warn!("microphone appears muted; cancelling");
                self.cancel().await;
                self.feedback.play(Sound::Error);
                self.notifier
                    .error("Microphone is muted or produced no audio")
                    .await;
                self.state.update(|snapshot| {
                    snapshot.message = "microphone muted".into();
                });
                return true;
            }
        } else {
            self.silence_since = None;
        }
        false
    }

    async fn check_vad(&mut self, elapsed: Duration) {
        let raw = self.capture.raw_level();
        let Some(vad) = self.vad.as_mut() else { return };
        if vad.push(raw, elapsed) != VadState::SegmentComplete {
            return;
        }
        vad.reset();

        if self.config.general.recording_mode == RecordingMode::Continuous {
            let segment = self.capture.drain();
            if !segment.is_empty() {
                tracing::debug!("continuous mode flushing {} samples", segment.len());
                self.queue(segment, true);
            }
        } else {
            tracing::info!("auto-stopping after silence");
            self.stop(false).await;
        }
    }

    async fn on_audio(&mut self, event: AudioEvent) {
        match event {
            AudioEvent::Started => tracing::debug!("capture stream running"),
            AudioEvent::Stopped => {}
            AudioEvent::DeviceLost(reason) => {
                tracing::warn!("capture device lost: {reason}");
                if self.phase != SessionPhase::Idle {
                    self.cancel().await;
                    self.feedback.play(Sound::Error);
                    self.notifier
                        .error("Microphone disconnected during recording")
                        .await;
                }
                self.capture.reconnect();
            }
            AudioEvent::Error(message) => {
                tracing::error!("capture error: {message}");
                self.state.update(|snapshot| {
                    snapshot.phase = Phase::Error;
                    snapshot.message = message.clone();
                });
                self.capture.reconnect();
            }
        }
    }

    async fn on_suspend(&mut self, sleeping: bool) {
        if sleeping {
            tracing::info!("system suspending");
            if self.phase != SessionPhase::Idle {
                self.cancel().await;
            }
            return;
        }
        tracing::info!("system resumed; refreshing audio and backend");
        self.capture.reconnect();
        let backend = Arc::clone(&self.backend);
        let state = self.state.clone();
        tokio::spawn(async move {
            if let Err(err) = backend.load().await {
                tracing::warn!("backend refresh after resume failed: {err:#}");
                state.update(|snapshot| {
                    snapshot.ready = false;
                    snapshot.message = format!("{err:#}");
                });
            }
        });
    }

    async fn on_command(&mut self, command: Command) -> bool {
        let Command { request, reply } = command;
        let mut shutdown = false;

        let response = match request {
            Request::Ping => Response::Ok,
            Request::Status => Response::Status(Box::new(self.state.get())),
            Request::Subscribe => Response::error("subscribe is handled by the connection"),
            Request::Toggle { language } => {
                let action = if self.phase == SessionPhase::Idle {
                    Action::Start { language }
                } else {
                    Action::Stop
                };
                self.apply(action).await;
                Response::Ok
            }
            Request::Start { language } => {
                self.start(language).await;
                Response::Ok
            }
            Request::Stop => {
                self.stop(false).await;
                Response::Ok
            }
            Request::Cancel => {
                self.cancel().await;
                Response::Ok
            }
            Request::Submit => {
                self.stop(true).await;
                Response::Ok
            }
            Request::Pause => {
                self.pause().await;
                Response::Ok
            }
            Request::Resume => {
                self.resume().await;
                Response::Ok
            }
            Request::Reload => match self.reload().await {
                Ok(()) => Response::Ok,
                Err(err) => Response::error(format!("{err:#}")),
            },
            Request::SetBackend { id } => self.set_backend(&id).await,
            Request::SetModel { name } => self.set_model(&name).await,
            Request::ModelUnload => self.unload_model().await,
            Request::ModelReload => self.load_model().await,
            Request::ModelToggle => {
                if self.backend.is_ready() {
                    self.unload_model().await
                } else {
                    self.load_model().await
                }
            }
            Request::TranscribeFile { path } => self.transcribe_file(&path).await,
            Request::Shutdown => {
                shutdown = true;
                Response::Ok
            }
        };

        let _ = reply.send(response);
        shutdown
    }

    async fn unload_model(&mut self) -> Response {
        match self.backend.unload().await {
            Ok(()) => {
                self.state.update(|snapshot| {
                    snapshot.ready = false;
                    snapshot.message = "model unloaded".into();
                });
                Response::Ok
            }
            Err(err) => Response::error(format!("{err:#}")),
        }
    }

    async fn load_model(&mut self) -> Response {
        match self.backend.load().await {
            Ok(()) => {
                self.state.update(|snapshot| {
                    snapshot.ready = true;
                    snapshot.message = "ready".into();
                    snapshot.phase = Phase::Idle;
                });
                Response::Ok
            }
            Err(err) => Response::error(format!("{err:#}")),
        }
    }

    async fn set_backend(&mut self, id: &str) -> Response {
        let mut config = Config::load_or_default();
        config.asr.backend = id.to_string();
        match asr::build(&config) {
            Ok(_) => {
                if let Err(err) = config.save() {
                    return Response::error(format!("{err:#}"));
                }
                self.config = Arc::new(config);
                Response::Text {
                    text: format!(
                        "backend set to '{id}'; restart the daemon to load it \
                         (systemctl --user restart duskr)"
                    ),
                }
            }
            Err(err) => Response::error(format!("{err:#}")),
        }
    }

    async fn set_model(&mut self, name: &str) -> Response {
        let mut config = Config::load_or_default();
        config.asr.whisper.model = name.to_string();
        if let Err(err) = config.save() {
            return Response::error(format!("{err:#}"));
        }
        self.config = Arc::new(config);
        Response::Text {
            text: format!(
                "model set to '{name}'; restart the daemon to load it \
                 (systemctl --user restart duskr)"
            ),
        }
    }

    async fn transcribe_file(&mut self, path: &str) -> Response {
        let path = std::path::PathBuf::from(path);
        let (samples, rate) = match crate::audio::wav::read_file(&path) {
            Ok(loaded) => loaded,
            Err(err) => return Response::error(format!("{err:#}")),
        };
        let request = asr::TranscribeRequest {
            samples: &samples,
            sample_rate: rate,
            language: self.config.general.language.as_deref(),
            prompt: Some(&self.config.asr.whisper.prompt),
        };
        match self.backend.transcribe(request).await {
            Ok(transcript) => Response::Text {
                text: transcribe::finish(
                    &transcript.text,
                    &self.config,
                    &transcript.backend,
                    Some(&self.state),
                )
                .await,
            },
            Err(err) => Response::error(format!("{err:#}")),
        }
    }

    async fn reload(&mut self) -> Result<()> {
        let new_config = Config::load()?;
        let hardware_changed = new_config.shortcuts != self.config.shortcuts
            || new_config.audio.device != self.config.audio.device
            || new_config.asr != self.config.asr;

        self.config = Arc::new(new_config);
        self.writer = StateWriter::new(&self.config.integrations);

        let mode = self.config.general.recording_mode;
        self.state.update(|snapshot| {
            snapshot.mode = mode.as_str().to_string();
            snapshot.language = self.config.general.language.clone();
        });

        if hardware_changed {
            tracing::info!(
                "configuration reloaded; restart the daemon to apply shortcut, \
                 device or backend changes"
            );
        } else {
            tracing::info!("configuration reloaded");
        }
        Ok(())
    }

    fn update_osd(&mut self, snapshot: &Snapshot) {
        let Some(osd) = &self.osd else {
            return;
        };
        let suppressed = self.state.watchers() > 0;
        if suppressed != self.osd_suppressed {
            self.osd_suppressed = suppressed;
            osd.set_suppressed(suppressed);
        }
        osd.update(snapshot.phase, snapshot.level);
    }

    fn run_state_hook(&self, snapshot: &Snapshot) {
        let Some(command) = self.config.integrations.state_hook.clone() else {
            return;
        };
        if command.trim().is_empty() {
            return;
        }
        let Ok(json) =
            serde_json::to_string(&crate::integrations::statefiles::status_file(snapshot))
        else {
            return;
        };
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let mut child = match tokio::process::Command::new("sh")
                .arg("-c")
                .arg(&command)
                .stdin(std::process::Stdio::piped())
                .spawn()
            {
                Ok(child) => child,
                Err(err) => {
                    tracing::debug!("state hook failed to start: {err}");
                    return;
                }
            };
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(json.as_bytes()).await;
            }
            let _ = child.wait().await;
        });
    }

    async fn shutdown(&mut self, shutdown_tx: &broadcast::Sender<()>) {
        if self.phase != SessionPhase::Idle {
            self.cancel().await;
        }
        self.state.emit(Event::Shutdown);
        let _ = shutdown_tx.send(());
        if let Some(hotkeys) = &self.hotkeys {
            hotkeys.stop();
        }
        if let Some(ducker) = &self.ducker {
            ducker.restore();
        }
        self.notifier.clear().await;
        self.writer.cleanup();
        let _ = std::fs::remove_file(paths::pid_file());
        tokio::time::sleep(Duration::from_millis(120)).await;
    }
}
