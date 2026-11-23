//! Turning symbols into sound and back: speed levels, the preamble, control
//! packets, and the receiver that finds a preamble and reads the syllables
//! after it. Nothing here touches a sound card.

use crate::dsp::{self, Frame, Rng, HOP, SR};
use crate::synth::Alphabet;
use std::sync::OnceLock;

/// A speed level: 1 gets through almost anything, 10 is for clean links.
pub struct Level {
    /// More pitches carry more bits per syllable, but sit closer together.
    pub pitches: usize,
    /// Longer syllables are easier to read and leave echoes time to die.
    pub ms: usize,
    /// Error-correction bytes added per data byte.
    pub redundancy: f64,
}

pub const LEVELS: [Level; 10] = [
    Level { pitches: 4, ms: 120, redundancy: 1.00 },
    Level { pitches: 8, ms: 100, redundancy: 0.70 },
    Level { pitches: 8, ms: 80, redundancy: 0.50 },
    Level { pitches: 16, ms: 80, redundancy: 0.40 },
    Level { pitches: 16, ms: 60, redundancy: 0.30 },
    Level { pitches: 16, ms: 50, redundancy: 0.25 },
    Level { pitches: 32, ms: 60, redundancy: 0.25 },
    Level { pitches: 32, ms: 50, redundancy: 0.20 },
    Level { pitches: 64, ms: 55, redundancy: 0.20 },
    Level { pitches: 64, ms: 50, redundancy: 0.15 },
];

pub fn level(n: usize) -> &'static Level {
    &LEVELS[n - 1]
}

/// The syllables of a level (1..=10), built once.
pub fn alphabet(n: usize) -> &'static Alphabet {
    static ALPHABETS: [OnceLock<Alphabet>; 10] = [const { OnceLock::new() }; 10];
    ALPHABETS[n - 1].get_or_init(|| Alphabet::new(level(n).pitches, level(n).ms))
}

/// Control packets always use the slowest, most robust level.
pub fn control() -> &'static Alphabet {
    alphabet(1)
}

/// Payload bits per second at a level with a given amount of error correction.
pub fn speed(n: usize, redundancy: f64) -> f64 {
    let l = level(n);
    (l.pitches.trailing_zeros() as f64) * 1000.0 / l.ms as f64 / (1.0 + redundancy)
}

// ------------------------------------------------------------- the preamble

const PREAMBLE_MS: usize = 100;
const LEAD_SYLLABLES: usize = 2;
const SYNC_SYLLABLES: usize = 12;
const SYNC_THRESHOLD: f32 = 0.4;

struct Preamble {
    /// Lead-in syllables (sacrificial: suppressors take a moment to settle),
    /// then the sync syllables the receiver looks for.
    audio: Vec<f32>,
    sync_len: usize,
    tail: Vec<f32>,
    feature: Vec<f32>,
    frames: usize,
}

fn preamble() -> &'static Preamble {
    static PREAMBLE: OnceLock<Preamble> = OnceLock::new();
    PREAMBLE.get_or_init(|| {
        let rich = Alphabet::new(8, PREAMBLE_MS);
        let mut rng = Rng::new(99);
        let mut syllables = Vec::new();
        for i in 0..LEAD_SYLLABLES + SYNC_SYLLABLES + 1 {
            syllables.push(rich.render(rng.below(8), 5000 + i));
        }
        let audio: Vec<f32> = syllables[..LEAD_SYLLABLES + SYNC_SYLLABLES].iter().flat_map(|s| s.iter().copied()).collect();
        let sync = &audio[LEAD_SYLLABLES * rich.n..];
        let mel = dsp::smear(&dsp::melspec(sync, HOP));
        Preamble {
            sync_len: sync.len(),
            tail: syllables.last().unwrap().to_vec(),
            feature: dsp::feature(&mel),
            frames: mel.len(),
            audio,
        }
    })
}

// ---------------------------------------------------------- control packets

/// The protocol this build speaks.
pub const PROTOCOL: u8 = 1;
/// The oldest receiver that can understand what this build sends. A receiver
/// cannot know whether a newer sender is still compatible with it, so the
/// sender says.
pub const MIN_RECEIVER: u8 = 1;
/// The oldest sender this build can receive from.
pub const MIN_SENDER: u8 = 1;

/// Kinds of control packet. The sender says CALL, RAMP, TEST, DATA, PROBE and
/// BYE; a receiver, only ever in negotiate mode, says the rest.
pub mod kind {
    pub const CALL: u8 = 1;
    pub const ANSWER: u8 = 2;
    pub const RAMP: u8 = 3;
    pub const REPORT: u8 = 4;
    pub const TEST: u8 = 5;
    pub const ACCEPT: u8 = 6;
    pub const DATA: u8 = 7;
    pub const ACK: u8 = 8;
    pub const NAK: u8 = 9;
    pub const PROBE: u8 = 10;
    pub const BYE: u8 = 11;
    pub const REFUSE: u8 = 12;

    /// Packets from the sender carry more than replies do.
    pub fn from_sender(kind: u8) -> bool {
        matches!(kind, CALL | RAMP | TEST | DATA | PROBE | BYE)
    }

    pub fn is_reply(kind: u8) -> bool {
        matches!(kind, ANSWER | REPORT | ACCEPT | ACK | NAK | REFUSE)
    }
}

/// Control syllables (two bits each) in a reply: kind, two arguments and an
/// eight-bit check.
pub const REPLY_SYLLABLES: usize = 10;
/// ...and in a packet from the sender, which adds flags, the two protocol
/// versions and the receiver's address.
pub const SENDER_SYLLABLES: usize = 20;

fn check(fields: &[u8]) -> u8 {
    let mut crc = 0x5Au8;
    for &v in fields {
        crc ^= v << 4 | (v ^ 0x9);
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 { crc << 1 ^ 0x07 } else { crc << 1 };
        }
    }
    crc
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Packet {
    pub kind: u8,
    pub a: u8,
    pub b: u8,
    /// From the sender only: what it is doing (encrypting, negotiating, streaming).
    pub flags: u8,
    /// From the sender only: the protocol it speaks.
    pub version: u8,
    /// From the sender only: the oldest receiver protocol that can follow it.
    pub needs: u8,
    /// From the sender only: which receiver this is for; 0 means anyone.
    pub address: u8,
}

impl Packet {
    /// A packet from the sender.
    pub fn sender(kind: u8, a: usize, b: usize, flags: u8, address: u8) -> Packet {
        Packet { kind, a: a as u8 & 15, b: b as u8 & 15, flags: flags & 15, version: PROTOCOL, needs: MIN_RECEIVER, address }
    }

    /// A reply from the receiver.
    pub fn reply(kind: u8, a: usize, b: usize) -> Packet {
        Packet { kind, a: a as u8 & 15, b: b as u8 & 15, flags: 0, version: 0, needs: 0, address: 0 }
    }

    fn fields(&self) -> Vec<u8> {
        if kind::from_sender(self.kind) {
            vec![self.kind, self.a, self.b, self.flags, self.version & 15, self.needs & 15, self.address >> 4, self.address & 15]
        } else {
            vec![self.kind, self.a, self.b]
        }
    }

    fn digits(&self) -> Vec<usize> {
        let mut fields = self.fields();
        let c = check(&fields);
        fields.extend([c >> 4, c & 15]);
        fields.iter().flat_map(|&f| [(f >> 2) as usize, (f & 3) as usize]).collect()
    }

    /// How many control syllables a packet has, judging by its first two
    /// (which give the kind). None if they are not the start of any packet.
    pub fn length(first: &[usize]) -> Option<usize> {
        let kind = (first.first()? << 2 | first.get(1)?) as u8;
        if kind::from_sender(kind) {
            Some(SENDER_SYLLABLES)
        } else if kind::is_reply(kind) {
            Some(REPLY_SYLLABLES)
        } else {
            None
        }
    }

    /// From the control syllables as heard, if the check passes.
    pub fn parse(digits: &[usize]) -> Option<Packet> {
        if Packet::length(digits)? != digits.len() {
            return None;
        }
        let f: Vec<u8> = digits.chunks(2).map(|d| (d[0] << 2 | d[1]) as u8).collect();
        let (body, sum) = f.split_at(f.len() - 2);
        if check(body) != sum[0] << 4 | sum[1] {
            return None;
        }
        Some(if body.len() == 8 {
            Packet { kind: f[0], a: f[1], b: f[2], flags: f[3], version: f[4], needs: f[5], address: f[6] << 4 | f[7] }
        } else {
            Packet { kind: f[0], a: f[1], b: f[2], flags: 0, version: 0, needs: 0, address: 0 }
        })
    }

    /// The sound of this packet, followed without a break by `sections` of
    /// data syllables, each in its own alphabet.
    pub fn audio(&self, sections: &[(&Alphabet, &[usize])]) -> Vec<f32> {
        let p = preamble();
        let mut out = p.audio.clone();
        for (i, d) in self.digits().into_iter().enumerate() {
            out.extend_from_slice(&control().render(d, i));
        }
        for (alphabet, symbols) in sections {
            for (i, &s) in symbols.iter().enumerate() {
                out.extend_from_slice(&alphabet.render(s, i));
            }
        }
        out.extend_from_slice(&p.tail);
        out.extend(std::iter::repeat(0.0).take(SR / 10));
        out
    }
}

/// Seconds a bare control packet takes.
pub fn packet_seconds(from_sender: bool) -> f64 {
    let p = preamble();
    let syllables = if from_sender { SENDER_SYLLABLES } else { REPLY_SYLLABLES };
    (p.audio.len() + syllables * control().n + p.tail.len() + SR / 10) as f64 / SR as f64
}

// ------------------------------------------------------- bits and syllables

/// Bytes -> syllable indexes of `bits` bits each (the last one zero-padded).
pub fn pack(data: &[u8], bits: usize) -> Vec<usize> {
    let mut out = Vec::with_capacity(data.len() * 8 / bits + 1);
    let (mut acc, mut have) = (0usize, 0usize);
    for &byte in data {
        acc = acc << 8 | byte as usize;
        have += 8;
        while have >= bits {
            out.push(acc >> (have - bits) & ((1 << bits) - 1));
            have -= bits;
        }
    }
    if have > 0 {
        out.push(acc << (bits - have) & ((1 << bits) - 1));
    }
    out
}

/// Syllables needed for `nbytes` bytes.
pub fn syllables_for(nbytes: usize, bits: usize) -> usize {
    (8 * nbytes).div_ceil(bits)
}

/// Syllable indexes -> bytes, each with the confidence of its weakest syllable.
pub fn unpack(symbols: &[(usize, f32)], bits: usize, nbytes: usize) -> (Vec<u8>, Vec<f32>) {
    let mut data = Vec::with_capacity(nbytes);
    let mut conf = Vec::with_capacity(nbytes);
    for i in 0..nbytes {
        let (first, last) = (8 * i / bits, (8 * i + 7) / bits);
        let mut value = 0usize;
        let mut weakest = f32::MAX;
        for s in first..=last {
            let (symbol, c) = symbols.get(s).copied().unwrap_or((0, 0.0));
            value = value << bits | symbol;
            weakest = weakest.min(c);
        }
        let extra = (last + 1) * bits - (8 * i + 8);
        data.push((value >> extra) as u8);
        conf.push(weakest);
    }
    (data, conf)
}

// ------------------------------------------------------------- the receiver

const FINE: usize = 80; // 5 ms: the step for timing adjustments

/// Feed it audio; it finds a preamble, then reads the syllables after it.
pub struct Receiver {
    buf: Vec<f32>,
    mel: Vec<Frame>,
    tested: usize,
    best: Option<(f32, usize)>,
    /// Sample at which the next syllable starts, once a preamble is found.
    pos: Option<usize>,
    drift: f64,
    aligned: bool,
}

impl Default for Receiver {
    fn default() -> Self {
        Self::new()
    }
}

impl Receiver {
    pub fn new() -> Receiver {
        Receiver { buf: Vec::new(), mel: Vec::new(), tested: 0, best: None, pos: None, drift: 0.0, aligned: false }
    }

    pub fn feed(&mut self, samples: &[f32]) {
        self.buf.extend_from_slice(samples);
    }

    pub fn locked(&self) -> bool {
        self.pos.is_some()
    }

    /// Look for a preamble in what has been fed so far. True once found.
    pub fn search(&mut self) -> bool {
        if self.pos.is_some() {
            return true;
        }
        let p = preamble();
        let start = self.mel.len() * HOP;
        if self.buf.len() >= start + dsp::WIN {
            self.mel.extend(dsp::melspec(&self.buf[start..], HOP));
        }
        if self.mel.len() >= p.frames {
            let recent = &self.mel[self.mel.len().saturating_sub(800)..];
            let floor = 0.01 * dsp::mean_power(recent);
            for t in self.tested..=self.mel.len() - p.frames {
                let window = dsp::unit(&dsp::logmel(&self.mel[t..t + p.frames], floor));
                let score = dsp::dot(&p.feature, &window);
                if score > SYNC_THRESHOLD && self.best.map_or(true, |(s, _)| score > s) {
                    self.best = Some((score, t));
                }
            }
            self.tested = self.mel.len() - p.frames + 1;
        }
        if let Some((_, t)) = self.best {
            // Wait 300 ms for anything better, then pin the start down finely.
            if self.mel.len() >= t + p.frames + 30 {
                let mut best: Option<(f32, usize)> = None;
                for shift in (-(HOP as i64)..=HOP as i64).step_by(40) {
                    let at = (t * HOP) as i64 + shift;
                    if at < 0 || at as usize + p.sync_len > self.buf.len() {
                        continue;
                    }
                    let at = at as usize;
                    let score = dsp::dot(&p.feature, &dsp::feature(&dsp::melspec(&self.buf[at..at + p.sync_len], HOP)));
                    if best.map_or(true, |(s, _)| score > s) {
                        best = Some((score, at));
                    }
                }
                self.best = None;
                if let Some((_, at)) = best {
                    self.pos = Some(at + p.sync_len);
                    self.drift = 0.0;
                    self.aligned = false;
                    return true;
                }
            }
        } else if self.buf.len() > 30 * SR {
            // Idle: keep only the last ten seconds.
            let drop = (self.buf.len() - 10 * SR) / HOP * HOP;
            self.buf.drain(..drop);
            self.mel.drain(..drop / HOP);
            self.tested = self.tested.saturating_sub(drop / HOP);
        }
        false
    }

    /// Forget the current packet and listen for the next preamble, keeping
    /// whatever audio has arrived after it.
    pub fn restart(&mut self) {
        let keep_from = self.pos.unwrap_or(self.buf.len()).saturating_sub(SR / 4).min(self.buf.len());
        self.buf.drain(..keep_from);
        self.mel.clear();
        self.tested = 0;
        self.best = None;
        self.pos = None;
    }

    /// Call before reading a run of syllables in a new alphabet: the timing
    /// is re-aligned on the first syllables of each run.
    pub fn begin_run(&mut self) {
        self.aligned = false;
    }

    fn clarity(&self, alphabet: &Alphabet, at: usize, position: usize) -> Vec<f64> {
        alphabet.pitch_scores(&self.buf[at..at + alphabet.n], position)
    }

    /// The next syllable of a run: (symbol, confidence), or None if more
    /// audio is needed first. `position` counts from 0 within the run and
    /// `count` is the run's length.
    ///
    /// Timing follows the pitch itself: how cleanly a pitch stands out is the
    /// best clock available, since every syllable has a different spectrum
    /// but a perfectly steady pitch.
    pub fn next_symbol(&mut self, alphabet: &Alphabet, position: usize, count: usize) -> Option<(usize, f32)> {
        let pos = self.pos?;
        let n = alphabet.n;
        if !self.aligned {
            let probe = count.min(20);
            if self.buf.len() < pos + probe * n + 4 * FINE + dsp::WIN {
                return None;
            }
            let mut best = (f64::MIN, 0i64);
            for step in -4..=4i64 {
                let shift = step * FINE as i64;
                if (pos as i64 + shift) < 0 {
                    continue;
                }
                let start = (pos as i64 + shift) as usize;
                let quality: f64 = (0..probe)
                    .map(|i| self.clarity(alphabet, start + i * n, position + i).into_iter().fold(f64::MIN, f64::max))
                    .sum();
                if quality > best.0 {
                    best = (quality, shift);
                }
            }
            self.pos = Some((pos as i64 + best.1) as usize);
            self.drift = 0.0;
            self.aligned = true;
        }
        let pos = self.pos?;
        let centre = pos as i64 + self.drift as i64;
        if centre < FINE as i64 || self.buf.len() < centre as usize + n + FINE {
            if centre < FINE as i64 {
                self.drift += FINE as f64;
            }
            return None;
        }
        let mut best: Option<(f64, i64, Vec<f64>)> = None;
        for lag in -1..=1i64 {
            let scores = self.clarity(alphabet, (centre + lag * FINE as i64) as usize, position);
            let top = scores.iter().copied().fold(f64::MIN, f64::max);
            if best.as_ref().map_or(true, |b| top > b.0) {
                best = Some((top, lag, scores));
            }
        }
        let (top, lag, scores) = best?;
        let symbol = scores.iter().position(|&s| s == top).unwrap_or(0);
        let second = scores.iter().enumerate().filter(|&(i, _)| i != symbol).map(|(_, &s)| s).fold(f64::MIN, f64::max);
        let confidence = if top > 0.0 { ((top - second) / top) as f32 } else { 0.0 };
        self.pos = Some(pos + n);
        self.drift += 0.15 * (lag * FINE as i64) as f64;
        Some((symbol, confidence))
    }

    /// Loudness in dB of the last `samples` before the current position.
    pub fn level_db(&self, samples: usize) -> f32 {
        let end = self.pos.unwrap_or(self.buf.len()).min(self.buf.len());
        let seg = &self.buf[end.saturating_sub(samples)..end];
        if seg.is_empty() {
            return -180.0;
        }
        let power = seg.iter().map(|v| v * v).sum::<f32>() / seg.len() as f32;
        (10.0 * (power + 1e-18).log10()).max(-180.0)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A crude room: echoes trailing over 70 ms, plus background noise.
    pub fn room(x: &[f32], snr_db: f64, seed: u64) -> Vec<f32> {
        let mut rng = Rng::new(seed);
        let n = SR * 7 / 100;
        let mut ir: Vec<f64> = (0..n).map(|i| rng.gaussian() * (-(i as f64) / (0.018 * SR as f64)).exp()).collect();
        let energy: f64 = ir.iter().map(|v| v * v).sum();
        ir.iter_mut().for_each(|v| *v *= (0.5 / energy).sqrt());
        ir[0] = 1.0;
        let mut y = vec![0.0f64; x.len()];
        for (i, &v) in x.iter().enumerate() {
            if v != 0.0 {
                for (k, h) in ir.iter().enumerate() {
                    if i + k < y.len() {
                        y[i + k] += v as f64 * h;
                    }
                }
            }
        }
        let loud: Vec<f64> = y.iter().copied().filter(|v| v.abs() > 0.01).collect();
        let power = loud.iter().map(|v| v * v).sum::<f64>() / loud.len().max(1) as f64;
        let sigma = (power / 10f64.powf(snr_db / 10.0)).sqrt();
        let mut out: Vec<f32> = (0..SR / 2).map(|_| (0.3 * sigma * rng.gaussian()) as f32).collect();
        out.extend(y.iter().map(|v| (0.3 * (v + sigma * rng.gaussian())) as f32));
        out
    }

    fn read_all(audio: &[f32], alphabet: &Alphabet, count: usize) -> Option<(Packet, Vec<usize>)> {
        let mut rx = Receiver::new();
        let mut chunks = audio.chunks(1600);
        let mut pull = |rx: &mut Receiver| match chunks.next() {
            Some(c) => {
                rx.feed(c);
                true
            }
            None => false,
        };
        while !rx.search() {
            if !pull(&mut rx) {
                return None;
            }
        }
        let mut read = |rx: &mut Receiver, a: &Alphabet, count: usize| -> Option<Vec<usize>> {
            rx.begin_run();
            let mut out = Vec::new();
            while out.len() < count {
                match rx.next_symbol(a, out.len(), count) {
                    Some((s, _)) => out.push(s),
                    None => {
                        if !pull(rx) {
                            return None;
                        }
                    }
                }
            }
            Some(out)
        };
        let digits = read(&mut rx, control(), SENDER_SYLLABLES)?;
        let packet = Packet::parse(&digits)?;
        Some((packet, read(&mut rx, alphabet, count)?))
    }

    #[test]
    fn packets_round_trip_through_a_room_at_every_level() {
        let mut rng = Rng::new(4);
        for lv in 1..=10 {
            let a = alphabet(lv);
            let symbols: Vec<usize> = (0..80).map(|_| rng.below(a.pitches.len())).collect();
            let packet = Packet::sender(kind::DATA, lv, 9, 5, 0xA7);
            let mut audio = vec![0.0f32; SR];
            audio.extend(packet.audio(&[(a, &symbols)]));
            audio.extend(vec![0.0f32; SR]);
            let (got, heard) = read_all(&room(&audio, 20.0, lv as u64), a, symbols.len()).expect("no packet");
            assert_eq!(got, packet);
            let wrong = heard.iter().zip(&symbols).filter(|(a, b)| a != b).count();
            // The top level is expected to be marginal in an echoing room.
            let allowed = if lv == 10 { 8 } else { 2 };
            assert!(wrong <= allowed, "level {lv}: {wrong} of {} syllables wrong", symbols.len());
        }
    }

    #[test]
    fn noise_alone_is_not_a_packet() {
        let mut rng = Rng::new(8);
        let noise: Vec<f32> = (0..SR * 20).map(|_| (0.1 * rng.gaussian()) as f32).collect();
        assert!(read_all(&noise, alphabet(6), 10).is_none());
    }

    #[test]
    fn bits_pack_and_unpack() {
        let data: Vec<u8> = (0..=255).collect();
        for bits in 2..=6 {
            let symbols = pack(&data, bits);
            assert_eq!(symbols.len(), syllables_for(data.len(), bits));
            let heard: Vec<(usize, f32)> = symbols.iter().map(|&s| (s, 1.0)).collect();
            assert_eq!(unpack(&heard, bits, data.len()).0, data);
        }
    }

    #[test]
    fn damaged_packets_are_rejected() {
        for packet in [Packet::sender(kind::RAMP, 3, 10, 1, 200), Packet::reply(kind::ACK, 5, 0)] {
            let good = packet.digits();
            assert_eq!(Packet::length(&good), Some(good.len()));
            assert_eq!(Packet::parse(&good), Some(packet));
            for i in 0..good.len() {
                let mut bad = good.clone();
                bad[i] ^= 1;
                assert!(Packet::parse(&bad).is_none());
            }
        }
        assert_eq!(Packet::sender(kind::CALL, 0, 0, 0, 0).digits().len(), SENDER_SYLLABLES);
        assert_eq!(Packet::reply(kind::ANSWER, 0, 0).digits().len(), REPLY_SYLLABLES);
        assert_eq!(Packet::length(&[0, 0]), None);
    }
}
