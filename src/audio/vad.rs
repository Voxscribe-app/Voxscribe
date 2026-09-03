//! Energy-based voice activity detection.
//!
//! Used for automatic/continuous mode and for auto-stop-on-silence. A neural
//! VAD would be more precise, but this runs in the capture thread's budget and
//! only has to answer "is the speaker still talking".

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadState {
    /// No speech heard yet this session.
    Waiting,
    Speaking,
    /// Speech has stopped but the silence window has not elapsed.
    Trailing,
    /// Silence window elapsed after speech: the segment is complete.
    SegmentComplete,
}

#[derive(Debug, Clone)]
pub struct Vad {
    threshold: f32,
    /// Silence required after speech before a segment is closed.
    silence: Duration,
    /// Speech required before the detector will arm; rejects key clicks.
    min_speech: Duration,
    state: VadState,
    speech_elapsed: Duration,
    silence_elapsed: Duration,
    /// Rolling noise floor, used when `threshold` is auto-calibrated.
    noise_floor: f32,
    calibrating: bool,
    calibration_elapsed: Duration,
}

/// Silence must sit this far above the measured noise floor to count as speech.
const NOISE_FLOOR_MARGIN: f32 = 3.0;
const CALIBRATION_WINDOW: Duration = Duration::from_millis(500);
const MIN_AUTO_THRESHOLD: f32 = 0.004;

impl Vad {
    /// `threshold` of 0 auto-calibrates from the first half second of audio.
    pub fn new(threshold: f32, silence: Duration, min_speech: Duration) -> Self {
        Self {
            threshold: if threshold > 0.0 {
                threshold
            } else {
                MIN_AUTO_THRESHOLD
            },
            silence,
            min_speech,
            state: VadState::Waiting,
            speech_elapsed: Duration::ZERO,
            silence_elapsed: Duration::ZERO,
            noise_floor: 0.0,
            calibrating: threshold <= 0.0,
            calibration_elapsed: Duration::ZERO,
        }
    }

    pub fn state(&self) -> VadState {
        self.state
    }

    pub fn threshold(&self) -> f32 {
        self.threshold
    }

    /// True once speech has been detected at least once this session.
    pub fn heard_speech(&self) -> bool {
        !matches!(self.state, VadState::Waiting)
    }

    pub fn reset(&mut self) {
        self.state = VadState::Waiting;
        self.speech_elapsed = Duration::ZERO;
        self.silence_elapsed = Duration::ZERO;
    }

    /// Feed one buffer's RMS and the duration it covered.
    pub fn push(&mut self, rms: f32, dt: Duration) -> VadState {
        if self.calibrating {
            // Track the quietest level seen while calibrating: the user is not
            // expected to stay silent, so a minimum beats an average.
            self.noise_floor = if self.calibration_elapsed.is_zero() {
                rms
            } else {
                self.noise_floor.min(rms)
            };
            self.calibration_elapsed += dt;
            if self.calibration_elapsed >= CALIBRATION_WINDOW {
                self.threshold = (self.noise_floor * NOISE_FLOOR_MARGIN).max(MIN_AUTO_THRESHOLD);
                self.calibrating = false;
            }
        }

        let loud = rms >= self.threshold;
        match self.state {
            VadState::Waiting => {
                if loud {
                    self.speech_elapsed += dt;
                    if self.speech_elapsed >= self.min_speech {
                        self.state = VadState::Speaking;
                        self.silence_elapsed = Duration::ZERO;
                    }
                } else {
                    self.speech_elapsed = Duration::ZERO;
                }
            }
            VadState::Speaking | VadState::Trailing => {
                if loud {
                    self.state = VadState::Speaking;
                    self.silence_elapsed = Duration::ZERO;
                } else {
                    self.silence_elapsed += dt;
                    self.state = if self.silence_elapsed >= self.silence {
                        VadState::SegmentComplete
                    } else {
                        VadState::Trailing
                    };
                }
            }
            // Terminal until the caller acknowledges by resetting.
            VadState::SegmentComplete => {}
        }
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICK: Duration = Duration::from_millis(100);

    fn vad() -> Vad {
        Vad::new(0.05, Duration::from_millis(500), Duration::from_millis(200))
    }

    fn feed(vad: &mut Vad, rms: f32, ticks: usize) -> VadState {
        let mut state = vad.state();
        for _ in 0..ticks {
            state = vad.push(rms, TICK);
        }
        state
    }

    #[test]
    fn brief_noise_does_not_arm_the_detector() {
        let mut vad = vad();
        assert_eq!(feed(&mut vad, 0.9, 1), VadState::Waiting);
        assert_eq!(feed(&mut vad, 0.0, 1), VadState::Waiting);
        assert!(!vad.heard_speech());
    }

    #[test]
    fn sustained_speech_arms_the_detector() {
        let mut vad = vad();
        assert_eq!(feed(&mut vad, 0.9, 3), VadState::Speaking);
        assert!(vad.heard_speech());
    }

    #[test]
    fn a_segment_closes_only_after_the_full_silence_window() {
        let mut vad = vad();
        feed(&mut vad, 0.9, 3);
        assert_eq!(feed(&mut vad, 0.0, 4), VadState::Trailing);
        assert_eq!(feed(&mut vad, 0.0, 1), VadState::SegmentComplete);
    }

    #[test]
    fn speech_resuming_mid_pause_cancels_the_pending_segment() {
        let mut vad = vad();
        feed(&mut vad, 0.9, 3);
        feed(&mut vad, 0.0, 3);
        assert_eq!(feed(&mut vad, 0.9, 1), VadState::Speaking);
        assert_eq!(feed(&mut vad, 0.0, 4), VadState::Trailing);
    }

    #[test]
    fn silence_before_any_speech_never_completes_a_segment() {
        let mut vad = vad();
        assert_eq!(feed(&mut vad, 0.0, 50), VadState::Waiting);
    }

    #[test]
    fn auto_calibration_raises_the_threshold_to_clear_the_noise_floor() {
        let mut vad = Vad::new(0.0, Duration::from_millis(500), Duration::from_millis(100));
        feed(&mut vad, 0.02, 6);
        assert!(
            vad.threshold() >= 0.02 * NOISE_FLOOR_MARGIN - f32::EPSILON,
            "threshold {} did not clear the noise floor",
            vad.threshold()
        );
        // Room tone at the calibrated level must no longer read as speech.
        vad.reset();
        assert_eq!(feed(&mut vad, 0.02, 10), VadState::Waiting);
    }

    #[test]
    fn a_completed_segment_stays_completed_until_reset() {
        let mut vad = vad();
        feed(&mut vad, 0.9, 3);
        feed(&mut vad, 0.0, 6);
        assert_eq!(feed(&mut vad, 0.9, 5), VadState::SegmentComplete);
        vad.reset();
        assert_eq!(vad.state(), VadState::Waiting);
    }
}
