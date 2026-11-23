//! The sound: short synthetic syllables whose pitch carries the data.
//!
//! Each syllable is a steady pitch (a buzz of harmonics, like vocal cords)
//! shaped by resonances (like the mouth shaping a vowel). The pitch is the
//! data: a room changes loudness and tone, but it cannot move a frequency.
//!
//! Everything else about a syllable changes from one to the next in a fixed
//! pattern that both ends know and that carries nothing: the vowel, how the
//! syllable starts, how bright and how loud it is. Without that variety, an
//! adaptive noise suppressor builds a profile of the sound and removes it
//! within seconds.

use crate::dsp::{self, Rng, SR};
use std::collections::HashMap;

/// (first formant, second formant) in Hz: eight vowels spread across the mouth.
const VOWELS: [(f64, f64); 8] = [
    (280.0, 2300.0), (800.0, 1250.0), (300.0, 800.0), (500.0, 1400.0),
    (450.0, 2050.0), (600.0, 900.0), (700.0, 1700.0), (300.0, 1600.0),
];

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Onset {
    Plain,
    /// A hiss centred on this many Hz before the voice starts.
    Hiss(u32),
    /// A short silence, then a burst.
    Stop,
}

const ONSETS: [Onset; 4] = [Onset::Plain, Onset::Hiss(5500), Onset::Stop, Onset::Hiss(3000)];
/// Fourth formant as (Hz, strength): how bright the syllable sounds.
const BRIGHT: [(f64, f64); 3] = [(3500.0, 0.35), (4300.0, 0.7), (3100.0, 0.7)];
const LOUD: [f64; 3] = [1.0, 0.8, 0.63];
pub const PITCH_LOW: f64 = 120.0;
pub const PITCH_HIGH: f64 = 210.0;

/// The data-free part of a syllable.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Decoration {
    vowel: u8,
    onset: u8,
    bright: u8,
    loud: u8,
}

impl Decoration {
    /// The decoration of the syllable at `position` in a run of syllables.
    pub fn at(position: usize) -> Decoration {
        let mut rng = Rng::new(0xDEC0 ^ (position as u64).wrapping_mul(0x9E37_79B9));
        Decoration {
            vowel: rng.below(VOWELS.len()) as u8,
            onset: rng.below(ONSETS.len()) as u8,
            bright: rng.below(BRIGHT.len()) as u8,
            loud: rng.below(LOUD.len()) as u8,
        }
    }

    pub fn onset(&self) -> Onset {
        ONSETS[self.onset as usize]
    }
}

/// `n` samples of noise concentrated around a frequency, peak 1.
fn noise(rng: &mut Rng, n: usize, centre: f64, width: f64) -> Vec<f64> {
    let size = n.next_power_of_two();
    let mut re: Vec<f64> = (0..size).map(|_| rng.gaussian()).collect();
    let mut im = vec![0.0; size];
    dsp::fft(&mut re, &mut im, false);
    for k in 0..size {
        let bin = if k <= size / 2 { k } else { size - k };
        let f = bin as f64 * SR as f64 / size as f64;
        let gain = (-0.5 * ((f - centre) / width).powi(2)).exp();
        re[k] *= gain;
        im[k] *= gain;
    }
    dsp::fft(&mut re, &mut im, true);
    re.truncate(n);
    let peak = re.iter().fold(1e-9f64, |m, v| m.max(v.abs()));
    re.iter_mut().for_each(|v| *v /= peak);
    re
}

/// Samples before the voice starts in a syllable of `n` samples with a hiss or stop.
pub fn onset_len(n: usize) -> usize {
    (n * 3 / 10).min(SR * 35 / 1000)
}

/// Samples over which the voice fades out at the end of a syllable, so the
/// room's echo of one pitch has died down before the next begins.
pub fn tail_len(n: usize) -> usize {
    n * 18 / 100
}

/// One syllable of `n` samples at `pitch` Hz.
pub fn syllable(pitch: f64, deco: Decoration, n: usize) -> Vec<f32> {
    let seed = (pitch * 100.0) as u64 ^ (deco.vowel as u64) << 20 ^ (deco.onset as u64) << 24
        ^ (deco.bright as u64) << 28 ^ (deco.loud as u64) << 32 ^ (n as u64) << 36;
    let mut rng = Rng::new(seed);
    let (f1, f2) = VOWELS[deco.vowel as usize];
    let (f4, f4_gain) = BRIGHT[deco.bright as usize];
    let onset = deco.onset();
    let on = onset_len(n);
    let tail = tail_len(n);

    // Loudness of the voiced part.
    let start = if onset == Onset::Plain { 0 } else { on };
    let mut env: Vec<f64> = (0..n)
        .map(|t| ((t as f64 - start as f64) / (0.008 * SR as f64)).clamp(0.0, 1.0))
        .collect();
    for i in 0..tail {
        env[n - tail + i] *= (1.0 - i as f64 / (tail - 1).max(1) as f64).powf(1.5);
    }

    // A buzz of harmonics, each weighted by the vowel's resonances.
    let mut phase = Vec::with_capacity(n);
    let mut acc = 0.0;
    let mut top = 0.0f64;
    for t in 0..n {
        let f0 = pitch * (1.0 + 0.004 * (2.0 * std::f64::consts::PI * t as f64 / n as f64).sin());
        top = top.max(f0);
        acc += 2.0 * std::f64::consts::PI * f0 / SR as f64;
        phase.push(acc);
    }
    let mut x = vec![0.0f64; n];
    for k in 1..=(7000.0 / top) as usize {
        let f = k as f64 * pitch;
        let shape = 1.0 / (1.0f64).hypot((f - f1) / 55.0)
            + 0.8 / (1.0f64).hypot((f - f2) / 75.0)
            + 0.5 / (1.0f64).hypot((f - 2700.0) / 110.0)
            + f4_gain / (1.0f64).hypot((f - f4) / 150.0);
        let gain = shape / k as f64;
        for t in 0..n {
            x[t] += gain * (k as f64 * phase[t]).sin();
        }
    }
    for t in 0..n {
        x[t] *= env[t];
    }
    let peak = x.iter().fold(1e-9f64, |m, v| m.max(v.abs()));

    // Breath: real voices are noisy above about 2.5 kHz, not purely harmonic.
    let breath = noise(&mut rng, n, 4200.0, 1500.0);
    for t in 0..n {
        x[t] += 0.15 * peak * env[t] * breath[t];
    }
    let mut add_noise = |at: usize, len: usize, centre: f64, level: f64, x: &mut Vec<f64>| {
        let len = len.min(n - at);
        let shaped = noise(&mut rng, len, centre, 1100.0);
        let window = dsp::hann(len);
        for i in 0..len {
            x[at + i] += level * peak * window[i] * shaped[i];
        }
    };
    match onset {
        Onset::Hiss(centre) => add_noise(0, on + 80, centre as f64, 0.9, &mut x),
        Onset::Stop => add_noise(on.saturating_sub(60), 200, 3500.0, 0.8, &mut x),
        Onset::Plain => {}
    }

    let rms = (x.iter().map(|v| v * v).sum::<f64>() / n as f64).sqrt() + 1e-9;
    let gain = 0.2 / rms * LOUD[deco.loud as usize];
    x.iter().map(|v| (v * gain).clamp(-0.95, 0.95) as f32).collect()
}

/// A set of pitches and a syllable length: one "speed level" of the modem.
pub struct Alphabet {
    pub pitches: Vec<f64>,
    pub ms: usize,
    /// Samples per syllable.
    pub n: usize,
    cache: std::sync::Mutex<HashMap<(usize, Decoration), std::sync::Arc<Vec<f32>>>>,
}

impl Alphabet {
    pub fn new(count: usize, ms: usize) -> Alphabet {
        let pitches = (0..count)
            .map(|i| PITCH_LOW * (PITCH_HIGH / PITCH_LOW).powf(i as f64 / (count - 1).max(1) as f64))
            .collect();
        Alphabet { pitches, ms, n: ms * SR / 1000, cache: Default::default() }
    }

    /// Bits carried by one syllable.
    pub fn bits(&self) -> usize {
        self.pitches.len().trailing_zeros() as usize
    }

    /// The sound of `symbol` when it is the `position`-th syllable of a run.
    pub fn render(&self, symbol: usize, position: usize) -> std::sync::Arc<Vec<f32>> {
        let deco = Decoration::at(position);
        let mut cache = self.cache.lock().unwrap();
        cache
            .entry((symbol, deco))
            .or_insert_with(|| std::sync::Arc::new(syllable(self.pitches[symbol], deco, self.n)))
            .clone()
    }

    /// Where in a syllable the steady pitch can be measured.
    fn voiced(&self, position: usize) -> (usize, usize) {
        let start = if Decoration::at(position).onset() == Onset::Plain { 0 } else { onset_len(self.n) };
        let end = self.n - tail_len(self.n) / 2;
        (start + (end - start) / 20, end)
    }

    /// How well each pitch explains a received syllable: a comb at that
    /// pitch's harmonics, minus a comb halfway between them.
    pub fn pitch_scores(&self, segment: &[f32], position: usize) -> Vec<f64> {
        const NFFT: usize = 8192;
        let (a, b) = self.voiced(position);
        let window = dsp::hann(b - a);
        let piece: Vec<f64> = (a..b).map(|i| segment[i] as f64 * window[i - a]).collect();
        let mag = dsp::magnitudes(&piece, NFFT);
        let total: f64 = mag.iter().sum::<f64>() + 1e-9;
        let hz = SR as f64 / NFFT as f64;
        self.pitches
            .iter()
            .map(|&f0| {
                let mut score = 0.0;
                for k in 1..=(4000.0 / f0) as usize {
                    score += mag[(k as f64 * f0 / hz).round() as usize];
                    score -= mag[((k as f64 - 0.5) * f0 / hz).round() as usize];
                }
                score / total
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syllables_are_deterministic_and_bounded() {
        let a = Alphabet::new(16, 50);
        let x = a.render(5, 3);
        assert_eq!(x.len(), 800);
        assert_eq!(*x, *Alphabet::new(16, 50).render(5, 3));
        assert!(x.iter().all(|v| v.abs() <= 0.95));
        assert!(x.iter().map(|v| v * v).sum::<f32>() > 1.0);
    }

    #[test]
    fn pitch_is_readable_from_a_clean_syllable() {
        for (count, ms) in [(4, 120), (16, 50), (32, 50), (64, 55)] {
            let a = Alphabet::new(count, ms);
            for position in 0..40 {
                let symbol = (position * 7) % count;
                let scores = a.pitch_scores(&a.render(symbol, position), position);
                let best = (0..count).max_by(|&i, &j| scores[i].partial_cmp(&scores[j]).unwrap()).unwrap();
                assert_eq!(best, symbol, "{count} pitches, {ms} ms, position {position}");
            }
        }
    }

    #[test]
    fn decoration_varies() {
        let seen: std::collections::HashSet<Decoration> = (0..200).map(Decoration::at).collect();
        assert!(seen.len() > 100);
    }
}
