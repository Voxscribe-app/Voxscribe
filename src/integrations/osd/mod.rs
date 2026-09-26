pub mod render;
mod surface;

use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::core::config::{Osd as OsdConfig, OsdMode};
use crate::core::state::Phase;
use render::{Canvas, Color, Layout, Renderer, Theme};
use surface::{Placement, Window};

const TICK: Duration = Duration::from_millis(50);
const IDLE_TICK: Duration = Duration::from_millis(250);
const FADE: Duration = Duration::from_millis(110);
const ERROR_SHOW: Duration = Duration::from_secs(3);

enum Message {
    Update { phase: Phase, level: f32 },
    Suppressed(bool),
    Stop,
}

pub struct Osd {
    tx: Sender<Message>,
    thread: Option<JoinHandle<()>>,
    mode: OsdMode,
}

impl Osd {
    pub fn start(config: &OsdConfig) -> Option<Self> {
        if config.enabled == OsdMode::Off {
            return None;
        }
        let placement = Placement {
            position: config.position,
            margin: config.margin,
            width: config.width,
            height: config.height,
        };
        let renderer = Renderer::new(theme_from(config), layout_from(config));
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("voxscribe-osd".into())
            .spawn(move || {
                let window = match Window::open(placement) {
                    Ok(window) => {
                        let _ = ready_tx.send(None);
                        window
                    }
                    Err(err) => {
                        let _ = ready_tx.send(Some(format!("{err:#}")));
                        return;
                    }
                };
                run(window, renderer, rx);
            })
            .ok()?;

        match ready_rx.recv() {
            Ok(None) => {}
            Ok(Some(err)) => {
                tracing::debug!("native OSD unavailable: {err}");
                let _ = thread.join();
                return None;
            }
            Err(_) => return None,
        }
        tracing::info!("native OSD ready");
        Some(Self {
            tx,
            thread: Some(thread),
            mode: config.enabled,
        })
    }

    pub fn update(&self, phase: Phase, level: f32) {
        let _ = self.tx.send(Message::Update { phase, level });
    }

    pub fn set_suppressed(&self, suppressed: bool) {
        if self.mode == OsdMode::Auto {
            let _ = self.tx.send(Message::Suppressed(suppressed));
        }
    }
}

impl Drop for Osd {
    fn drop(&mut self) {
        let _ = self.tx.send(Message::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn theme_from(config: &OsdConfig) -> Theme {
    let defaults = Theme::default();
    let background = Color::parse(&config.background)
        .unwrap_or(defaults.background)
        .with_alpha(config.opacity);
    Theme {
        border: background.lighter(0.18).with_alpha(config.opacity),
        background,
        accent: Color::parse(&config.accent).unwrap_or(defaults.accent),
        ..defaults
    }
}

fn layout_from(config: &OsdConfig) -> Layout {
    let defaults = Layout::default();
    Layout {
        width: config.width as f32,
        height: config.height as f32,
        radius: config.radius as f32,
        bars: config.bars,
        bar_max: (config.height as f32 - 2.0 * defaults.gap).max(defaults.bar_min + 1.0),
        ..defaults
    }
}

fn wants_visible(phase: Phase, since_change: Duration, suppressed: bool) -> bool {
    if suppressed {
        return false;
    }
    match phase {
        Phase::Recording | Phase::Paused | Phase::Processing => true,
        Phase::Error => since_change < ERROR_SHOW,
        _ => false,
    }
}

fn run(mut window: Window, mut renderer: Renderer, rx: mpsc::Receiver<Message>) {
    let mut phase = Phase::Starting;
    let mut level = 0.0f32;
    let mut suppressed = false;
    let mut changed_at = Instant::now();
    let mut opacity = 0.0f32;
    let mut canvas: Option<Canvas> = None;
    let mut last_tick = Instant::now();

    loop {
        if let Err(err) = window.pump() {
            tracing::debug!("OSD Wayland connection lost: {err:#}");
            return;
        }

        let visible = opacity > 0.0;
        let wait = if visible { TICK } else { IDLE_TICK };
        match rx.recv_timeout(wait) {
            Ok(Message::Update {
                phase: next,
                level: next_level,
            }) => {
                if next != phase {
                    phase = next;
                    changed_at = Instant::now();
                }
                level = next_level;
            }
            Ok(Message::Suppressed(next)) => suppressed = next,
            Ok(Message::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }

        let elapsed = last_tick.elapsed();
        if elapsed < TICK && opacity > 0.0 && opacity < 1.0 {
            continue;
        }
        last_tick = Instant::now();

        let step = (elapsed.as_secs_f32() / FADE.as_secs_f32()).clamp(0.0, 1.0);
        let target = wants_visible(phase, changed_at.elapsed(), suppressed);
        opacity = if target {
            (opacity + step).min(1.0)
        } else {
            (opacity - step).max(0.0)
        };

        if opacity <= 0.0 {
            if window.is_mapped() {
                window.hide();
                canvas = None;
            }
            continue;
        }

        if !window.is_mapped() {
            if let Err(err) = window.show() {
                tracing::debug!("OSD surface failed: {err:#}");
                return;
            }
            continue;
        }

        renderer.advance(phase, level);
        renderer.set_opacity(opacity);

        let scale = window.scale();
        let rebuild =
            window.take_dirty() || canvas.as_ref().is_none_or(|canvas| canvas.scale != scale);
        if rebuild {
            canvas = Some(Canvas::new(
                renderer.layout.width as u32,
                renderer.layout.height as u32,
                scale,
            ));
        }
        let Some(canvas) = canvas.as_mut() else {
            continue;
        };

        renderer.draw(canvas, phase);
        if let Err(err) = window.present(canvas) {
            tracing::debug!("OSD present failed: {err:#}");
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_active_phases_put_the_island_on_screen() {
        let now = Duration::ZERO;
        assert!(wants_visible(Phase::Recording, now, false));
        assert!(wants_visible(Phase::Processing, now, false));
        assert!(wants_visible(Phase::Paused, now, false));
        assert!(!wants_visible(Phase::Idle, now, false));
        assert!(!wants_visible(Phase::Starting, now, false));
    }

    #[test]
    fn a_sticky_error_phase_does_not_pin_the_island_open() {
        assert!(wants_visible(Phase::Error, Duration::ZERO, false));
        assert!(!wants_visible(
            Phase::Error,
            ERROR_SHOW + Duration::from_millis(1),
            false
        ));
    }

    #[test]
    fn suppression_wins_over_every_phase() {
        assert!(!wants_visible(Phase::Recording, Duration::ZERO, true));
    }

    #[test]
    fn the_layout_follows_the_configured_geometry() {
        let config = OsdConfig {
            width: 240,
            height: 44,
            bars: 24,
            ..OsdConfig::default()
        };
        let layout = layout_from(&config);
        assert_eq!(layout.width, 240.0);
        assert_eq!(layout.bars, 24);
        assert!(layout.bar_max > Layout::default().bar_min);
    }

    #[test]
    fn opacity_is_folded_into_the_island_colors() {
        let config = OsdConfig {
            opacity: 0.5,
            background: "#000000".into(),
            accent: "#ff0000".into(),
            ..OsdConfig::default()
        };
        let theme = theme_from(&config);
        assert_eq!(theme.background.a, 0.5);
        assert_eq!(theme.accent, Color::rgba(1.0, 0.0, 0.0, 1.0));
    }

    #[test]
    #[ignore = "requires a Wayland session with zwlr_layer_shell_v1"]
    fn manual_island_smoke_test() {
        let config = OsdConfig {
            enabled: OsdMode::On,
            ..OsdConfig::default()
        };
        let osd = Osd::start(&config).expect("no layer shell in this session");
        let start = Instant::now();
        let mut tick = 0.0f32;
        while start.elapsed() < Duration::from_secs(6) {
            osd.update(Phase::Recording, 0.35 + 0.35 * (tick / 6.0).sin());
            std::thread::sleep(Duration::from_millis(40));
            tick += 1.0;
        }
    }

    #[test]
    fn bad_colors_fall_back_instead_of_failing() {
        let config = OsdConfig {
            accent: "not-a-color".into(),
            ..OsdConfig::default()
        };
        assert_eq!(theme_from(&config).accent, Theme::default().accent);
    }
}
