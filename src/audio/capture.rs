use std::mem;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use pipewire as pw;
use pw::{properties::properties, spa};
use spa::param::format::{MediaSubtype, MediaType};
use spa::param::format_utils;
use spa::pod::Pod;
use tokio::sync::mpsc::UnboundedSender;

use crate::audio::level::LevelMeter;
use crate::core::config::Audio;

#[derive(Debug, Clone, PartialEq)]
pub enum AudioEvent {
    Started,
    Stopped,
    DeviceLost(String),
    Error(String),
}

struct Shared {
    level: AtomicU32,
    raw_level: AtomicU32,
    frames: AtomicU64,
    armed: AtomicBool,
    running: AtomicBool,
    negotiated_rate: AtomicU32,
    buffer: Mutex<Vec<f32>>,
    max_samples: AtomicU64,
}

impl Shared {
    fn set_level(&self, smoothed: f32, raw: f32) {
        self.level.store(smoothed.to_bits(), Ordering::Relaxed);
        self.raw_level.store(raw.to_bits(), Ordering::Relaxed);
    }
}

enum Command {
    Arm,
    Disarm,
    Reconnect,
    Quit,
}

pub struct Capture {
    sender: pw::channel::Sender<Command>,
    shared: Arc<Shared>,
    sample_rate: u32,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Capture {
    pub fn start(config: &Audio, events: UnboundedSender<AudioEvent>) -> Result<Self> {
        let shared = Arc::new(Shared {
            level: AtomicU32::new(0),
            raw_level: AtomicU32::new(0),
            frames: AtomicU64::new(0),
            armed: AtomicBool::new(false),
            running: AtomicBool::new(false),
            negotiated_rate: AtomicU32::new(config.sample_rate),
            buffer: Mutex::new(Vec::new()),
            max_samples: AtomicU64::new(
                config.max_recording_seconds as u64 * config.sample_rate as u64,
            ),
        });

        let (sender, receiver) = pw::channel::channel::<Command>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();

        let thread_shared = Arc::clone(&shared);
        let thread_config = config.clone();
        let handle = std::thread::Builder::new()
            .name("duskr-pipewire".into())
            .spawn(move || {
                if let Err(err) = run_loop(
                    thread_config,
                    thread_shared,
                    receiver,
                    events.clone(),
                    &ready_tx,
                ) {
                    let _ = ready_tx.send(Err(err));
                }
            })
            .context("spawning the PipeWire capture thread")?;

        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| anyhow!("PipeWire did not respond within 5s"))??;

        Ok(Self {
            sender,
            shared,
            sample_rate: config.sample_rate,
            handle: Some(handle),
        })
    }

    pub fn sample_rate(&self) -> u32 {
        let negotiated = self.shared.negotiated_rate.load(Ordering::Relaxed);
        if negotiated > 0 {
            negotiated
        } else {
            self.sample_rate
        }
    }

    pub fn arm(&self) {
        self.shared
            .buffer
            .lock()
            .expect("capture buffer poisoned")
            .clear();
        self.shared.frames.store(0, Ordering::SeqCst);
        self.shared.armed.store(true, Ordering::SeqCst);
        let _ = self.sender.send(Command::Arm);
    }

    pub fn disarm(&self) {
        self.shared.armed.store(false, Ordering::SeqCst);
        let _ = self.sender.send(Command::Disarm);
    }

    pub fn take(&self) -> Vec<f32> {
        self.shared.armed.store(false, Ordering::SeqCst);
        let mut buffer = self.shared.buffer.lock().expect("capture buffer poisoned");
        mem::take(&mut *buffer)
    }

    pub fn drain(&self) -> Vec<f32> {
        let mut buffer = self.shared.buffer.lock().expect("capture buffer poisoned");
        mem::take(&mut *buffer)
    }

    pub fn sample_count(&self) -> usize {
        self.shared
            .buffer
            .lock()
            .expect("capture buffer poisoned")
            .len()
    }

    pub fn level(&self) -> f32 {
        f32::from_bits(self.shared.level.load(Ordering::Relaxed))
    }

    pub fn raw_level(&self) -> f32 {
        f32::from_bits(self.shared.raw_level.load(Ordering::Relaxed))
    }

    pub fn frames(&self) -> u64 {
        self.shared.frames.load(Ordering::Relaxed)
    }

    pub fn is_running(&self) -> bool {
        self.shared.running.load(Ordering::SeqCst)
    }

    pub fn reconnect(&self) {
        let _ = self.sender.send(Command::Reconnect);
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.sender.send(Command::Quit);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct UserData {
    channels: u32,
    meter: LevelMeter,
    shared: Arc<Shared>,
}

fn build_format_pod(config: &Audio) -> Result<Vec<u8>> {
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(config.sample_rate);
    info.set_channels(1);

    let object = pw::spa::pod::Object {
        type_: pw::spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: pw::spa::param::ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };
    let bytes = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(object),
    )
    .map_err(|err| anyhow!("serializing the audio format: {err:?}"))?
    .0
    .into_inner();
    Ok(bytes)
}

fn run_loop(
    config: Audio,
    shared: Arc<Shared>,
    receiver: pw::channel::Receiver<Command>,
    events: UnboundedSender<AudioEvent>,
    ready: &std::sync::mpsc::Sender<Result<()>>,
) -> Result<()> {
    pw::init();

    let mainloop =
        pw::main_loop::MainLoopRc::new(None).context("creating the PipeWire main loop")?;
    let context =
        pw::context::ContextRc::new(&mainloop, None).context("creating the PipeWire context")?;
    let core = context.connect_rc(None).context("connecting to PipeWire")?;

    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Communication",
        *pw::keys::APP_NAME => "Duskr",
        *pw::keys::NODE_NAME => "duskr-capture",
    };
    if let Some(target) = config.device.as_ref().or(config.device_match.as_ref()) {
        props.insert("target.object", target.clone());
    }

    let stream = pw::stream::StreamRc::new(core.clone(), "duskr-capture", props)
        .context("creating the capture stream")?;

    let user_data = UserData {
        channels: 1,
        meter: LevelMeter::default(),
        shared: Arc::clone(&shared),
    };

    let state_events = events.clone();
    let state_shared = Arc::clone(&shared);
    let _listener = stream
        .add_local_listener_with_user_data(user_data)
        .state_changed(move |_, _, old, new| {
            tracing::debug!("capture stream {old:?} -> {new:?}");
            match new {
                pw::stream::StreamState::Streaming => {
                    state_shared.running.store(true, Ordering::SeqCst);
                    let _ = state_events.send(AudioEvent::Started);
                }
                pw::stream::StreamState::Error(err) => {
                    state_shared.running.store(false, Ordering::SeqCst);
                    let _ = state_events.send(AudioEvent::Error(err.to_string()));
                }
                pw::stream::StreamState::Unconnected => {
                    let was_running = state_shared.running.swap(false, Ordering::SeqCst);
                    if was_running {
                        let _ = state_events
                            .send(AudioEvent::DeviceLost("capture node disappeared".into()));
                    }
                }
                _ => {
                    state_shared.running.store(false, Ordering::SeqCst);
                    let _ = state_events.send(AudioEvent::Stopped);
                }
            }
        })
        .param_changed(|_, user_data, id, param| {
            let Some(param) = param else { return };
            if id != pw::spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Ok((media_type, media_subtype)) = format_utils::parse_format(param) else {
                return;
            };
            if media_type != MediaType::Audio || media_subtype != MediaSubtype::Raw {
                return;
            }
            let mut info = spa::param::audio::AudioInfoRaw::default();
            if info.parse(param).is_err() {
                return;
            }
            user_data.channels = info.channels().max(1);
            user_data
                .shared
                .negotiated_rate
                .store(info.rate(), Ordering::Relaxed);
            tracing::info!(
                "capturing at {} Hz, {} channel(s)",
                info.rate(),
                user_data.channels
            );
        })
        .process(|stream, user_data| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }
            let data = &mut datas[0];
            let size = data.chunk().size() as usize;
            let Some(bytes) = data.data() else { return };
            let bytes = &bytes[..size.min(bytes.len())];

            let samples: Vec<f32> = bytes
                .chunks_exact(mem::size_of::<f32>())
                .map(|chunk| f32::from_le_bytes(chunk.try_into().expect("4-byte chunk")))
                .collect();
            if samples.is_empty() {
                return;
            }

            let mono = if user_data.channels > 1 {
                crate::audio::wav::downmix(&samples, user_data.channels as usize)
            } else {
                samples
            };

            let smoothed = user_data.meter.push(&mono);
            user_data.shared.set_level(smoothed, user_data.meter.raw());
            user_data.shared.frames.fetch_add(1, Ordering::Relaxed);

            if user_data.shared.armed.load(Ordering::Relaxed) {
                let cap = user_data.shared.max_samples.load(Ordering::Relaxed) as usize;
                let mut sink = user_data
                    .shared
                    .buffer
                    .lock()
                    .expect("capture buffer poisoned");
                if sink.len() < cap {
                    let room = cap - sink.len();
                    sink.extend_from_slice(&mono[..mono.len().min(room)]);
                }
            }
        })
        .register()
        .context("registering the capture stream listener")?;

    let format = build_format_pod(&config)?;
    let mut params = [Pod::from_bytes(&format).ok_or_else(|| anyhow!("invalid format pod"))?];

    let stream_flags = pw::stream::StreamFlags::AUTOCONNECT
        | pw::stream::StreamFlags::MAP_BUFFERS
        | pw::stream::StreamFlags::RT_PROCESS;
    stream
        .connect(
            spa::utils::Direction::Input,
            None,
            stream_flags,
            &mut params,
        )
        .context("connecting the capture stream")?;

    if !config.keepalive {
        let _ = stream.set_active(false);
    }

    let _ready_sent = ready.send(Ok(()));

    let keepalive = config.keepalive;
    let loop_stream = stream.clone();
    let loop_handle = mainloop.clone();
    let loop_shared = Arc::clone(&shared);
    let reconnect_format = format.clone();
    let _receiver = receiver.attach(mainloop.loop_(), move |command| match command {
        Command::Arm => {
            let _ = loop_stream.set_active(true);
        }
        Command::Disarm => {
            if !keepalive {
                let _ = loop_stream.set_active(false);
            }
        }
        Command::Reconnect => {
            let _ = loop_stream.set_active(false);
            let _ = loop_stream.disconnect();
            if let Some(pod) = Pod::from_bytes(&reconnect_format) {
                let mut params = [pod];
                if let Err(error) = loop_stream.connect(
                    spa::utils::Direction::Input,
                    None,
                    stream_flags,
                    &mut params,
                ) {
                    tracing::warn!("capture reconnect failed: {error}");
                } else {
                    let active = keepalive || loop_shared.armed.load(Ordering::SeqCst);
                    let _ = loop_stream.set_active(active);
                }
            }
        }
        Command::Quit => loop_handle.quit(),
    });

    mainloop.run();
    let _ = stream.disconnect();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_negotiated_format_pod_is_serializable() {
        let config = Audio::default();
        let pod = build_format_pod(&config).unwrap();
        assert!(!pod.is_empty());
        assert!(Pod::from_bytes(&pod).is_some());
    }
}
