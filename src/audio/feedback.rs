use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use pipewire as pw;
use pw::{properties::properties, spa};
use spa::pod::Pod;

use crate::core::config::Audio;

pub const PLAYBACK_RATE: u32 = 48_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    Start,
    Stop,
    Error,
}

fn synthesize(sound: Sound) -> Vec<f32> {
    let (from, to, seconds) = match sound {
        Sound::Start => (660.0_f32, 990.0, 0.09),
        Sound::Stop => (880.0, 590.0, 0.09),
        Sound::Error => (300.0, 200.0, 0.22),
    };
    let total = (PLAYBACK_RATE as f32 * seconds) as usize;
    let mut out = Vec::with_capacity(total);
    let mut phase = 0.0_f32;
    for i in 0..total {
        let t = i as f32 / (total - 1).max(1) as f32;
        let frequency = from + (to - from) * t;
        phase += std::f32::consts::TAU * frequency / PLAYBACK_RATE as f32;
        let envelope = (std::f32::consts::PI * t).sin().max(0.0).powf(0.6);
        out.push(phase.sin() * envelope * 0.35);
    }
    out
}

fn load_file(path: &Path) -> Result<Vec<f32>> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    let (samples, rate) = match extension.as_str() {
        "ogg" | "oga" => decode_ogg(&bytes)?,
        _ => crate::audio::wav::decode(&bytes)?,
    };
    Ok(crate::audio::wav::resample(&samples, rate, PLAYBACK_RATE))
}

fn decode_ogg(bytes: &[u8]) -> Result<(Vec<f32>, u32)> {
    use lewton::inside_ogg::OggStreamReader;

    let mut reader = OggStreamReader::new(std::io::Cursor::new(bytes))
        .map_err(|err| anyhow!("reading Ogg Vorbis: {err}"))?;
    let channels = reader.ident_hdr.audio_channels.max(1) as usize;
    let rate = reader.ident_hdr.audio_sample_rate;

    let mut interleaved = Vec::new();
    while let Some(packet) = reader
        .read_dec_packet_itl()
        .map_err(|err| anyhow!("decoding Ogg Vorbis: {err}"))?
    {
        interleaved.extend(packet.into_iter().map(|s| s as f32 / i16::MAX as f32));
    }
    Ok((crate::audio::wav::downmix(&interleaved, channels), rate))
}

struct Queue {
    pending: Mutex<VecDeque<Vec<f32>>>,
    active: Mutex<Option<(Vec<f32>, usize)>>,
    playing: AtomicBool,
}

enum Command {
    Play,
    Quit,
}

pub struct Feedback {
    queue: Arc<Queue>,
    sender: Option<pw::channel::Sender<Command>>,
    handle: Option<std::thread::JoinHandle<()>>,
    enabled: bool,
    volume: f32,
    start: Vec<f32>,
    stop: Vec<f32>,
    error: Vec<f32>,
}

impl Feedback {
    pub fn new(config: &Audio) -> Self {
        let load = |path: &Option<std::path::PathBuf>, fallback: Sound| match path {
            Some(path) => match load_file(path) {
                Ok(samples) => samples,
                Err(err) => {
                    tracing::warn!("falling back to the built-in sound: {err:#}");
                    synthesize(fallback)
                }
            },
            None => synthesize(fallback),
        };

        let mut feedback = Self {
            queue: Arc::new(Queue {
                pending: Mutex::new(VecDeque::new()),
                active: Mutex::new(None),
                playing: AtomicBool::new(false),
            }),
            sender: None,
            handle: None,
            enabled: config.feedback,
            volume: config.volume.clamp(0.0, 1.0),
            start: load(&config.start_sound, Sound::Start),
            stop: load(&config.stop_sound, Sound::Stop),
            error: load(&config.error_sound, Sound::Error),
        };

        if feedback.enabled {
            match feedback.spawn() {
                Ok((sender, handle)) => {
                    feedback.sender = Some(sender);
                    feedback.handle = Some(handle);
                }
                Err(err) => {
                    tracing::warn!("audio feedback disabled: {err:#}");
                    feedback.enabled = false;
                }
            }
        }
        feedback
    }

    fn spawn(&self) -> Result<(pw::channel::Sender<Command>, std::thread::JoinHandle<()>)> {
        let (sender, receiver) = pw::channel::channel::<Command>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
        let queue = Arc::clone(&self.queue);

        let handle = std::thread::Builder::new()
            .name("duskr-feedback".into())
            .spawn(move || {
                if let Err(err) = playback_loop(queue, receiver, &ready_tx) {
                    let _ = ready_tx.send(Err(err));
                }
            })
            .context("spawning the feedback thread")?;

        ready_rx
            .recv_timeout(Duration::from_secs(3))
            .map_err(|_| anyhow!("PipeWire playback did not start"))??;
        Ok((sender, handle))
    }

    pub fn play(&self, sound: Sound) {
        if !self.enabled {
            return;
        }
        let Some(sender) = &self.sender else { return };

        let samples = match sound {
            Sound::Start => &self.start,
            Sound::Stop => &self.stop,
            Sound::Error => &self.error,
        };
        let scaled: Vec<f32> = samples.iter().map(|s| s * self.volume).collect();

        self.queue
            .pending
            .lock()
            .expect("feedback queue poisoned")
            .push_back(scaled);
        let _ = sender.send(Command::Play);
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
}

impl Drop for Feedback {
    fn drop(&mut self) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(Command::Quit);
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn playback_loop(
    queue: Arc<Queue>,
    receiver: pw::channel::Receiver<Command>,
    ready: &std::sync::mpsc::Sender<Result<()>>,
) -> Result<()> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    let props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Playback",
        *pw::keys::MEDIA_ROLE => "Notification",
        *pw::keys::APP_NAME => "Duskr",
        *pw::keys::NODE_NAME => "duskr-feedback",
    };

    let stream = pw::stream::StreamRc::new(core.clone(), "duskr-feedback", props)?;

    let process_queue = Arc::clone(&queue);
    let _listener = stream
        .add_local_listener_with_user_data(())
        .process(move |stream, _| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }
            let data = &mut datas[0];
            let stride = std::mem::size_of::<f32>();

            let Some(slice) = data.data() else { return };
            let capacity = slice.len() / stride;
            let mut written = 0usize;

            {
                let mut active = process_queue
                    .active
                    .lock()
                    .expect("feedback state poisoned");
                if active.is_none() {
                    *active = process_queue
                        .pending
                        .lock()
                        .expect("feedback queue poisoned")
                        .pop_front()
                        .map(|samples| (samples, 0));
                }

                if let Some((samples, offset)) = active.as_mut() {
                    let take = (samples.len() - *offset).min(capacity);
                    for i in 0..take {
                        let value = samples[*offset + i];
                        let start = i * stride;
                        slice[start..start + stride].copy_from_slice(&value.to_le_bytes());
                    }
                    *offset += take;
                    written = take;
                    if *offset >= samples.len() {
                        *active = None;
                    }
                }
            }

            for i in written..capacity {
                let start = i * stride;
                slice[start..start + stride].copy_from_slice(&0f32.to_le_bytes());
            }

            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = stride as i32;
            *chunk.size_mut() = (capacity * stride) as u32;

            let idle = written == 0
                && process_queue
                    .pending
                    .lock()
                    .expect("feedback queue poisoned")
                    .is_empty();
            process_queue.playing.store(!idle, Ordering::Relaxed);
        })
        .register()?;

    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(PLAYBACK_RATE);
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
    .map_err(|err| anyhow!("serializing the playback format: {err:?}"))?
    .0
    .into_inner();
    let mut params = [Pod::from_bytes(&bytes).ok_or_else(|| anyhow!("invalid format pod"))?];

    stream.connect(
        spa::utils::Direction::Output,
        None,
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::RT_PROCESS,
        &mut params,
    )?;
    let _ = stream.set_active(false);

    let _ = ready.send(Ok(()));

    let loop_stream = stream.clone();
    let loop_handle = mainloop.clone();
    let idle_queue = Arc::clone(&queue);

    let timer = mainloop.loop_().add_timer({
        let stream = stream.clone();
        move |_| {
            let idle = !idle_queue.playing.load(Ordering::Relaxed)
                && idle_queue
                    .pending
                    .lock()
                    .expect("feedback queue poisoned")
                    .is_empty()
                && idle_queue
                    .active
                    .lock()
                    .expect("feedback state poisoned")
                    .is_none();
            if idle {
                let _ = stream.set_active(false);
            }
        }
    });
    let _ = timer.update_timer(
        Some(Duration::from_millis(500)),
        Some(Duration::from_millis(500)),
    );

    let _receiver = receiver.attach(mainloop.loop_(), move |command| match command {
        Command::Play => {
            let _ = loop_stream.set_active(true);
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
    fn synthesized_pings_are_audible_and_click_free() {
        for sound in [Sound::Start, Sound::Stop, Sound::Error] {
            let samples = synthesize(sound);
            assert!(!samples.is_empty());
            assert!(samples[0].abs() < 1e-3, "{sound:?} starts with a click");
            assert!(
                samples[samples.len() - 1].abs() < 1e-3,
                "{sound:?} ends with a click"
            );
            let peak = samples.iter().fold(0.0f32, |acc, s| acc.max(s.abs()));
            assert!(peak > 0.1 && peak <= 1.0, "{sound:?} peak {peak}");
        }
    }

    #[test]
    fn start_and_stop_pings_are_distinguishable() {
        assert_ne!(synthesize(Sound::Start), synthesize(Sound::Stop));
    }

    #[test]
    fn a_wav_ping_is_loaded_and_resampled_to_the_playback_rate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ping.wav");
        let samples: Vec<f32> = (0..8_000).map(|i| (i as f32 / 20.0).sin() * 0.5).collect();
        crate::audio::wav::write_file(&path, &samples, 8_000).unwrap();

        let loaded = load_file(&path).unwrap();
        assert!(
            (loaded.len() as i64 - 48_000).abs() < 10,
            "{}",
            loaded.len()
        );
    }

    #[test]
    fn an_unreadable_sound_file_is_reported_rather_than_panicking() {
        assert!(load_file(Path::new("/nonexistent/ping.wav")).is_err());
    }
}
