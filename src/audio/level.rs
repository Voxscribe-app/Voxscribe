//! Capture level metering for UI integrations.

/// Exponentially smoothed RMS meter. Smoothing lives here so every consumer
/// does not repeat it; `raw` stays available for mute detection.
#[derive(Debug, Clone)]
pub struct LevelMeter {
    smoothed: f32,
    raw: f32,
    attack: f32,
    release: f32,
    /// Speech rarely exceeds 0.1 RMS, so a linear 0..1 meter would be flat.
    gain: f32,
}

impl Default for LevelMeter {
    fn default() -> Self {
        Self {
            smoothed: 0.0,
            raw: 0.0,
            attack: 0.6,
            release: 0.15,
            gain: 10.0,
        }
    }
}

impl LevelMeter {
    pub fn push(&mut self, samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return self.smoothed;
        }
        let sum: f32 = samples.iter().map(|s| s * s).sum();
        self.push_rms((sum / samples.len() as f32).sqrt())
    }

    pub fn push_rms(&mut self, rms: f32) -> f32 {
        self.raw = rms;
        let target = (rms * self.gain).clamp(0.0, 1.0);
        // Fast attack for onset, slow release so it does not strobe.
        let alpha = if target > self.smoothed {
            self.attack
        } else {
            self.release
        };
        self.smoothed += (target - self.smoothed) * alpha;
        if self.smoothed < 1e-4 {
            self.smoothed = 0.0;
        }
        self.smoothed
    }

    pub fn level(&self) -> f32 {
        self.smoothed
    }

    /// Unsmoothed RMS of the most recent buffer.
    pub fn raw(&self) -> f32 {
        self.raw
    }

    pub fn reset(&mut self) {
        self.smoothed = 0.0;
        self.raw = 0.0;
    }
}

/// Digital silence from a hardware-muted mic, far below room tone.
pub const DIGITAL_SILENCE_RMS: f32 = 5e-7;

pub fn is_digital_silence(rms: f32) -> bool {
    rms < DIGITAL_SILENCE_RMS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_reads_as_zero() {
        let mut meter = LevelMeter::default();
        assert_eq!(meter.push(&[0.0; 128]), 0.0);
    }

    #[test]
    fn the_meter_rises_faster_than_it_falls() {
        let mut meter = LevelMeter::default();
        meter.push(&[0.2; 128]);
        let after_attack = meter.level();
        assert!(after_attack > 0.5, "attack too slow: {after_attack}");

        meter.push(&[0.0; 128]);
        let after_release = meter.level();
        assert!(
            after_release > after_attack * 0.5,
            "release too fast: {after_attack} -> {after_release}"
        );
    }

    #[test]
    fn loud_input_saturates_at_one() {
        let mut meter = LevelMeter::default();
        for _ in 0..20 {
            meter.push(&[1.0; 128]);
        }
        assert_eq!(meter.level(), 1.0);
    }

    #[test]
    fn raw_rms_is_reported_unsmoothed_for_mute_detection() {
        let mut meter = LevelMeter::default();
        meter.push(&[0.5; 64]);
        assert!((meter.raw() - 0.5).abs() < 1e-6);
        assert!(!is_digital_silence(meter.raw()));

        meter.push(&[0.0; 64]);
        assert!(is_digital_silence(meter.raw()));
    }

    #[test]
    fn an_empty_buffer_does_not_disturb_the_meter() {
        let mut meter = LevelMeter::default();
        meter.push(&[0.3; 64]);
        let before = meter.level();
        assert_eq!(meter.push(&[]), before);
    }
}
