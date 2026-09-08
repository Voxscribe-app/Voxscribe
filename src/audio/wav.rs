use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result};

pub fn to_pcm16(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

pub fn from_pcm16(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(2)
        .map(|chunk| {
            let value = i16::from_le_bytes([chunk[0], chunk[1]]);
            value as f32 / i16::MAX as f32
        })
        .collect()
}

pub fn encode(samples: &[f32], sample_rate: u32) -> Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut buffer = Cursor::new(Vec::new());
    {
        let mut writer =
            hound::WavWriter::new(&mut buffer, spec).context("creating the WAV writer")?;
        for sample in samples {
            writer.write_sample((sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
        }
        writer.finalize().context("finalizing the WAV stream")?;
    }
    Ok(buffer.into_inner())
}

pub fn write_file(path: &Path, samples: &[f32], sample_rate: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, encode(samples, sample_rate)?)
        .with_context(|| format!("writing {}", path.display()))
}

pub fn decode(bytes: &[u8]) -> Result<(Vec<f32>, u32)> {
    let mut reader = hound::WavReader::new(Cursor::new(bytes)).context("reading WAV data")?;
    let spec = reader.spec();
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<Result<_, _>>()?
        }
    };
    Ok((
        downmix(&interleaved, spec.channels as usize),
        spec.sample_rate,
    ))
}

pub fn read_file(path: &Path) -> Result<(Vec<f32>, u32)> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    decode(&bytes)
}

pub fn downmix(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
        .collect()
}

pub fn resample(samples: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || from == 0 || samples.is_empty() {
        return samples.to_vec();
    }
    let ratio = to as f64 / from as f64;
    let out_len = ((samples.len() as f64) * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let position = i as f64 / ratio;
        let index = position.floor() as usize;
        let frac = (position - index as f64) as f32;
        let a = samples[index.min(samples.len() - 1)];
        let b = samples[(index + 1).min(samples.len() - 1)];
        out.push(a + (b - a) * frac);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm16_round_trips_within_quantization_error() {
        let samples = vec![0.0, 0.5, -0.5, 1.0, -1.0];
        let decoded = from_pcm16(&to_pcm16(&samples));
        assert_eq!(decoded.len(), samples.len());
        for (a, b) in samples.iter().zip(decoded.iter()) {
            assert!((a - b).abs() < 1e-4, "{a} != {b}");
        }
    }

    #[test]
    fn out_of_range_samples_clamp_instead_of_wrapping() {
        let encoded = to_pcm16(&[2.0, -2.0]);
        let decoded = from_pcm16(&encoded);
        assert!((decoded[0] - 1.0).abs() < 1e-4);
        assert!((decoded[1] + 1.0).abs() < 1e-4);
    }

    #[test]
    fn wav_round_trips_through_encode_and_decode() {
        let samples: Vec<f32> = (0..800).map(|i| (i as f32 / 16.0).sin() * 0.4).collect();
        let bytes = encode(&samples, 16_000).unwrap();
        let (decoded, rate) = decode(&bytes).unwrap();
        assert_eq!(rate, 16_000);
        assert_eq!(decoded.len(), samples.len());
        for (a, b) in samples.iter().zip(decoded.iter()) {
            assert!((a - b).abs() < 1e-3);
        }
    }

    #[test]
    fn stereo_input_is_averaged_to_mono() {
        assert_eq!(downmix(&[1.0, 0.0, 0.5, 0.5], 2), vec![0.5, 0.5]);
        assert_eq!(downmix(&[1.0, 2.0], 1), vec![1.0, 2.0]);
    }

    #[test]
    fn resampling_scales_length_and_is_a_noop_at_equal_rates() {
        let samples: Vec<f32> = (0..1000).map(|i| i as f32 / 1000.0).collect();
        assert_eq!(resample(&samples, 16_000, 16_000).len(), 1000);
        assert_eq!(resample(&samples, 48_000, 16_000).len(), 333);
        assert_eq!(resample(&samples, 8_000, 16_000).len(), 2000);
    }

    #[test]
    fn resampling_preserves_the_signal_shape() {
        let samples: Vec<f32> = (0..480).map(|i| (i as f32 * 0.01).sin()).collect();
        let downsampled = resample(&samples, 48_000, 16_000);
        for i in 0..downsampled.len().min(150) {
            assert!((downsampled[i] - samples[i * 3]).abs() < 0.02);
        }
    }

    #[test]
    fn a_truncated_pcm_buffer_drops_the_partial_frame() {
        assert_eq!(from_pcm16(&[0x00, 0x00, 0x11]).len(), 1);
    }
}
