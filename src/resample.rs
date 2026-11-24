//! Sample-rate conversion for audio as it streams past: sound cards rarely
//! run at vocsend's 16 kHz.

/// Converts a stream from one rate to another with a windowed-sinc filter.
pub struct Resampler {
    /// Input samples per output sample.
    ratio: f64,
    /// Half-width of the filter, in input samples.
    half: usize,
    /// Pass band, as a fraction of the input's Nyquist limit.
    cutoff: f64,
    history: Vec<f32>,
    /// Position in `history` of the next output sample.
    next: f64,
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Resampler {
        let ratio = from as f64 / to as f64;
        let half = (12.0 * ratio.max(1.0)).ceil() as usize;
        Resampler { ratio, half, cutoff: 0.92 * (1.0 / ratio).min(1.0), history: Vec::new(), next: half as f64 }
    }

    /// Feed input, get whatever output is ready. A little is held back each
    /// time, because every output sample depends on input either side of it.
    pub fn push(&mut self, input: &[f32]) -> Vec<f32> {
        if self.ratio == 1.0 {
            return input.to_vec();
        }
        self.history.extend_from_slice(input);
        let mut out = Vec::with_capacity((input.len() as f64 / self.ratio) as usize + 2);
        while self.next + (self.half as f64) < self.history.len() as f64 {
            let centre = self.next.floor() as usize;
            let mut sum = 0.0f64;
            for i in centre + 1 - self.half..=centre + self.half {
                let t = self.next - i as f64;
                let x = std::f64::consts::PI * self.cutoff * t;
                let sinc = if x.abs() < 1e-9 { 1.0 } else { x.sin() / x };
                let window = 0.5 + 0.5 * (std::f64::consts::PI * t / self.half as f64).cos();
                sum += self.history[i] as f64 * self.cutoff * sinc * window;
            }
            out.push(sum as f32);
            self.next += self.ratio;
        }
        let keep = (self.next.floor() as usize).saturating_sub(self.half);
        self.history.drain(..keep);
        self.next -= keep as f64;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, hz: f64, seconds: f64) -> Vec<f32> {
        (0..(rate as f64 * seconds) as usize)
            .map(|i| (2.0 * std::f64::consts::PI * hz * i as f64 / rate as f64).sin() as f32)
            .collect()
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len().max(1) as f64).sqrt()
    }

    #[test]
    fn keeps_tones_and_length_in_both_directions() {
        for (from, to) in [(16000, 48000), (48000, 16000), (16000, 44100), (44100, 16000), (16000, 16000)] {
            let mut r = Resampler::new(from, to);
            let input = tone(from, 1000.0, 1.0);
            let mut out = Vec::new();
            for chunk in input.chunks(480) {
                out.extend(r.push(chunk));
            }
            let expected = to as usize;
            assert!((out.len() as i64 - expected as i64).abs() < 200, "{from}->{to}: {} samples", out.len());
            let middle = &out[out.len() / 4..out.len() * 3 / 4];
            assert!((rms(middle) - 0.7071).abs() < 0.02, "{from}->{to}: level {}", rms(middle));
            // The tone must still be at 1 kHz: count zero crossings.
            let crossings = middle.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
            let hz = crossings as f64 / (middle.len() as f64 / to as f64);
            assert!((hz - 1000.0).abs() < 10.0, "{from}->{to}: {hz} Hz");
        }
    }

    #[test]
    fn removes_what_the_lower_rate_cannot_carry() {
        let mut r = Resampler::new(48000, 16000);
        let out = r.push(&tone(48000, 11000.0, 0.5));
        assert!(rms(&out[out.len() / 4..]) < 0.02, "an 11 kHz tone leaked through at {}", rms(&out));
    }
}
