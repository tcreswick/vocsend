//! Signal-processing basics: a random number generator, an FFT, and the
//! mel-spectrogram features used to find the preamble.

pub const SR: usize = 16000;
pub const WIN: usize = 400; // 25 ms analysis window
pub const NFFT: usize = 512;
pub const HOP: usize = 160; // 10 ms
pub const NBANDS: usize = 26;

pub type Frame = [f32; NBANDS];

/// A small deterministic generator (SplitMix64). Both ends must produce the
/// same sounds from the same seeds, so nothing here may depend on the platform.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0x1234_5678_9ABC_DEF0)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// Uniform in (0, 1).
    pub fn unit(&mut self) -> f64 {
        ((self.next() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }

    pub fn gaussian(&mut self) -> f64 {
        let (a, b) = (self.unit(), self.unit());
        (-2.0 * a.ln()).sqrt() * (2.0 * std::f64::consts::PI * b).cos()
    }
}

/// In-place radix-2 FFT. The length must be a power of two.
pub fn fft(re: &mut [f64], im: &mut [f64], inverse: bool) {
    let n = re.len();
    debug_assert!(n.is_power_of_two() && im.len() == n);
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let sign = if inverse { 1.0 } else { -1.0 };
    let mut len = 2;
    while len <= n {
        let angle = sign * 2.0 * std::f64::consts::PI / len as f64;
        let (wr, wi) = (angle.cos(), angle.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0, 0.0);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let tr = re[b] * cr - im[b] * ci;
                let ti = re[b] * ci + im[b] * cr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let next = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = next;
            }
        }
        len <<= 1;
    }
    if inverse {
        for v in re.iter_mut().chain(im.iter_mut()) {
            *v /= n as f64;
        }
    }
}

/// A periodic Hann window.
pub fn hann(n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos())
        .collect()
}

/// Magnitude spectrum of `x` (already windowed), zero-padded to `nfft`.
pub fn magnitudes(x: &[f64], nfft: usize) -> Vec<f64> {
    let mut re = vec![0.0; nfft];
    let mut im = vec![0.0; nfft];
    re[..x.len()].copy_from_slice(x);
    fft(&mut re, &mut im, false);
    (0..=nfft / 2).map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt()).collect()
}

/// Triangular mel filters between 150 Hz and 6 kHz, as (first bin, weights).
fn mel_filters() -> &'static Vec<(usize, Vec<f32>)> {
    static FILTERS: std::sync::OnceLock<Vec<(usize, Vec<f32>)>> = std::sync::OnceLock::new();
    FILTERS.get_or_init(|| {
        let mel = |f: f64| 2595.0 * (1.0 + f / 700.0).log10();
        let inv = |m: f64| 700.0 * (10f64.powf(m / 2595.0) - 1.0);
        let (lo, hi) = (mel(150.0), mel(6000.0));
        let edges: Vec<f64> = (0..NBANDS + 2)
            .map(|i| inv(lo + (hi - lo) * i as f64 / (NBANDS + 1) as f64))
            .collect();
        (0..NBANDS)
            .map(|b| {
                let (l, m, h) = (edges[b], edges[b + 1], edges[b + 2]);
                let mut first = None;
                let mut weights = Vec::new();
                for k in 0..=NFFT / 2 {
                    let f = k as f64 * SR as f64 / NFFT as f64;
                    let w = ((f - l) / (m - l)).min((h - f) / (h - m)).max(0.0);
                    if w > 0.0 {
                        first.get_or_insert(k);
                        weights.push(w as f32);
                    } else if first.is_some() {
                        break;
                    }
                }
                (first.unwrap_or(0), weights)
            })
            .collect()
    })
}

/// Mel power spectrogram: one frame of NBANDS values every `hop` samples.
pub fn melspec(x: &[f32], hop: usize) -> Vec<Frame> {
    if x.len() < WIN {
        return Vec::new();
    }
    static WINDOW: std::sync::OnceLock<Vec<f64>> = std::sync::OnceLock::new();
    let window = WINDOW.get_or_init(|| hann(WIN));
    let filters = mel_filters();
    let count = (x.len() - WIN) / hop + 1;
    let mut out = Vec::with_capacity(count);
    let mut re = vec![0.0; NFFT];
    let mut im = vec![0.0; NFFT];
    for f in 0..count {
        re.iter_mut().for_each(|v| *v = 0.0);
        im.iter_mut().for_each(|v| *v = 0.0);
        for i in 0..WIN {
            re[i] = x[f * hop + i] as f64 * window[i];
        }
        fft(&mut re, &mut im, false);
        let mut frame = [0f32; NBANDS];
        for (b, (first, weights)) in filters.iter().enumerate() {
            let mut sum = 0.0;
            for (i, w) in weights.iter().enumerate() {
                let k = first + i;
                sum += (re[k] * re[k] + im[k] * im[k]) * *w as f64;
            }
            frame[b] = sum as f32;
        }
        out.push(frame);
    }
    out
}

/// Bands above about 2.5 kHz survive a room far better than the low ones,
/// where echoes boost and cancel individual harmonics, so they count for more.
pub fn band_weight(b: usize) -> f32 {
    0.5 + b as f32 / (NBANDS - 1) as f32
}

/// A room smears sound in time: the share of energy arriving 0, 10, ... 60 ms late.
const SMEAR: [f32; 7] = [0.87, 0.05, 0.05, 0.01, 0.01, 0.005, 0.005];

pub fn smear(mel: &[Frame]) -> Vec<Frame> {
    let mut out = vec![[0f32; NBANDS]; mel.len()];
    for (k, h) in SMEAR.iter().enumerate() {
        for t in k..mel.len() {
            for b in 0..NBANDS {
                out[t][b] += h * mel[t - k][b];
            }
        }
    }
    out
}

pub fn mean_power(mel: &[Frame]) -> f32 {
    if mel.is_empty() {
        return 0.0;
    }
    mel.iter().map(|f| f.iter().sum::<f32>()).sum::<f32>() / (mel.len() * NBANDS) as f32
}

/// Log-mel with a floor, weighted per band.
pub fn logmel(mel: &[Frame], floor: f32) -> Vec<Frame> {
    mel.iter()
        .map(|f| {
            let mut out = [0f32; NBANDS];
            for b in 0..NBANDS {
                out[b] = (f[b] + floor + 1e-12).ln() * band_weight(b);
            }
            out
        })
        .collect()
}

/// Remove each band's mean over time, flatten, and scale to unit length.
pub fn unit(frames: &[Frame]) -> Vec<f32> {
    let n = frames.len().max(1) as f32;
    let mut mean = [0f32; NBANDS];
    for f in frames {
        for b in 0..NBANDS {
            mean[b] += f[b] / n;
        }
    }
    let mut flat = Vec::with_capacity(frames.len() * NBANDS);
    let mut norm = 0f32;
    for f in frames {
        for b in 0..NBANDS {
            let v = f[b] - mean[b];
            norm += v * v;
            flat.push(v);
        }
    }
    let scale = 1.0 / (norm.sqrt() + 1e-9);
    flat.iter_mut().for_each(|v| *v *= scale);
    flat
}

/// The matching feature of a stretch of audio: weighted log-mel, unit length.
pub fn feature(mel: &[Frame]) -> Vec<f32> {
    unit(&logmel(mel, 0.01 * mean_power(mel)))
}

pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_round_trip_and_tone() {
        let n = 1024;
        let mut re: Vec<f64> = (0..n).map(|i| (2.0 * std::f64::consts::PI * 37.0 * i as f64 / n as f64).sin()).collect();
        let orig = re.clone();
        let mut im = vec![0.0; n];
        fft(&mut re, &mut im, false);
        let peak = (0..n / 2).max_by(|&a, &b| (re[a].hypot(im[a])).partial_cmp(&re[b].hypot(im[b])).unwrap()).unwrap();
        assert_eq!(peak, 37);
        fft(&mut re, &mut im, true);
        assert!(re.iter().zip(&orig).all(|(a, b)| (a - b).abs() < 1e-9));
    }

    #[test]
    fn rng_is_stable() {
        let mut r = Rng::new(7);
        let a: Vec<u64> = (0..3).map(|_| r.next()).collect();
        let mut r = Rng::new(7);
        assert_eq!(a, (0..3).map(|_| r.next()).collect::<Vec<_>>());
        assert!((0..1000).map(|_| r.gaussian()).sum::<f64>().abs() < 150.0);
    }
}
