//! Decisions and arithmetic: how much error correction a level needs, how a
//! payload is cut into blocks, and how long it will all take.

use crate::crypto;
use crate::frame;
use crate::modem::{self, level};

/// Air time per acknowledged block. An acknowledgement costs several
/// seconds, so shorter blocks would waste a large share of the time.
pub const BLOCK_SECONDS: f64 = 30.0;
/// Bytes per block when nothing is acknowledged: in effect, one block.
pub const ONE_WAY_BLOCK: usize = 60000;
/// Pause before replying, so the other side has started listening.
pub const TURNAROUND: f64 = 0.5;
pub const RAMP_LEVELS: (usize, usize) = (3, 10);
pub const RAMP_SYLLABLES: usize = 12;
pub const TEST_SYLLABLES: usize = 60;
pub const ATTEMPTS: usize = 3;
/// Default without negotiation: with no replies there is no second chance.
pub const ONE_WAY_LEVEL: usize = 4;

/// The amounts of error correction a receiver can ask for.
pub const REDUNDANCY: [f64; 10] = [0.15, 0.2, 0.25, 0.3, 0.4, 0.5, 0.6, 0.7, 0.85, 1.0];

pub fn redundancy_code(redundancy: f64) -> usize {
    REDUNDANCY.iter().position(|&r| r >= redundancy - 1e-9).unwrap_or(REDUNDANCY.len() - 1)
}

/// Known syllables for a ramp (salt 1), test (2) or probe (3) at a level.
pub fn known_symbols(lv: usize, count: usize, salt: u64) -> Vec<usize> {
    let mut rng = crate::dsp::Rng::new(salt * 100 + lv as u64);
    (0..count).map(|_| rng.below(level(lv).pitches)).collect()
}

/// Share of bytes that will be wrong if this share of syllables is.
fn byte_errors(lv: usize, syllable_errors: f64) -> f64 {
    1.0 - (1.0 - syllable_errors).powf(8.0 / level(lv).pitches.trailing_zeros() as f64)
}

/// How much error correction a level needs, given a test result.
///
/// Reed-Solomon repairs a fixed share of each block for certain, so a link
/// with a few errors is usable as long as that share comfortably exceeds the
/// error rate. This allows for the test being a small sample and asks for
/// twice the repair capacity the measured rate implies. None: even 100%
/// would not be enough.
pub fn needed_redundancy(lv: usize, wrong: usize, count: usize) -> Option<f64> {
    let default = level(lv).redundancy;
    if wrong == 0 {
        return Some(default);
    }
    let bad = byte_errors(lv, (wrong + 1) as f64 / (count + 2) as f64);
    if 4.0 * bad >= 1.0 {
        return None;
    }
    let need = default.max(4.0 * bad / (1.0 - 4.0 * bad));
    REDUNDANCY.iter().copied().find(|&r| r >= need - 1e-9)
}

/// After the test: keep the level with enough error correction, or step
/// down, whichever delivers more. Returns (level, redundancy).
pub fn choose(lv: usize, wrong: usize) -> (usize, f64) {
    let mut options: Vec<(f64, usize, f64)> = Vec::new();
    let need = needed_redundancy(lv, wrong, TEST_SYLLABLES);
    if let Some(r) = need {
        options.push((modem::speed(lv, r), lv, r));
    }
    if lv > 1 && wrong > 0 {
        let lower = if need.is_some() { lv - 1 } else { lv.saturating_sub(2).max(1) };
        options.push((modem::speed(lower, level(lower).redundancy), lower, level(lower).redundancy));
    }
    options
        .into_iter()
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap())
        .map(|(_, l, r)| (l, r))
        .unwrap_or((1, level(1).redundancy))
}

/// Would real data get through at a level with its default error correction?
pub fn verdict(lv: usize, wrong_share: f64) -> &'static str {
    let r = level(lv).redundancy;
    let repairable = r / (2.0 * (1.0 + r));
    let bad = byte_errors(lv, wrong_share);
    if bad <= repairable / 3.0 {
        "solid"
    } else if bad <= repairable {
        "marginal"
    } else {
        "fails"
    }
}

/// Bytes of the stream carried by one block.
fn block_size(lv: usize, redundancy: f64, acknowledged: bool, encrypted: bool) -> usize {
    if !acknowledged {
        return ONE_WAY_BLOCK;
    }
    let on_air = (modem::speed(lv, redundancy) * BLOCK_SECONDS / 8.0) as usize;
    on_air.saturating_sub(if encrypted { crypto::OVERHEAD } else { 0 }).max(16)
}

/// Sizes of the blocks a stream of `nbytes` goes in. A small leftover rides
/// with the block before it: on its own it would cost more in preamble and
/// acknowledgement than it carries.
pub fn split(nbytes: usize, lv: usize, redundancy: f64, acknowledged: bool, encrypted: bool) -> Vec<usize> {
    let size = block_size(lv, redundancy, acknowledged, encrypted);
    let mut sizes = vec![size; nbytes / size];
    if nbytes % size > 0 {
        sizes.push(nbytes % size);
    }
    if sizes.len() > 1 && sizes[sizes.len() - 1] < size / 4 {
        let leftover = sizes.pop().unwrap();
        *sizes.last_mut().unwrap() += leftover;
    }
    if sizes.is_empty() {
        sizes.push(0);
    }
    sizes
}

/// Seconds of air time for one block, with the reply and pauses around it.
fn block_seconds(nbytes: usize, lv: usize, redundancy: f64, acknowledged: bool, encrypted: bool) -> f64 {
    let on_air = frame::encoded_len(nbytes + if encrypted { crypto::OVERHEAD } else { 0 }, redundancy);
    let alphabet = modem::alphabet(lv);
    let data = modem::syllables_for(on_air, alphabet.bits()) as f64 * alphabet.ms as f64 / 1000.0;
    let reply = modem::packet_seconds(false) + 2.0 * TURNAROUND + 0.3;
    modem::packet_seconds(true) + data + if acknowledged { reply } else { TURNAROUND }
}

/// (blocks, seconds) to send `nbytes` more if nothing has to be repeated.
pub fn estimate(nbytes: usize, lv: usize, redundancy: f64, acknowledged: bool, encrypted: bool) -> (usize, f64) {
    let sizes = split(nbytes, lv, redundancy, acknowledged, encrypted);
    let seconds = sizes.iter().map(|&n| block_seconds(n, lv, redundancy, acknowledged, encrypted)).sum();
    (sizes.len(), seconds)
}

pub fn clock(seconds: f64) -> String {
    let s = seconds.round() as u64;
    if s >= 60 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitting_accounts_for_every_byte() {
        for lv in 1..=10 {
            for &r in &[level(lv).redundancy, 1.0] {
                for n in (0..900).chain([5000, 70000]) {
                    for enc in [false, true] {
                        let sizes = split(n, lv, r, true, enc);
                        assert_eq!(sizes.iter().sum::<usize>(), n);
                        assert!(n == 0 || sizes.iter().all(|&s| s > 0));
                    }
                }
            }
        }
        assert_eq!(split(5000, 6, 0.25, false, false), [5000]);
    }

    #[test]
    fn a_marginal_level_keeps_going_with_more_correction() {
        assert_eq!(choose(8, 0), (8, 0.2));
        let (lv, r) = choose(8, 1);
        assert_eq!(lv, 8);
        assert!(r > 0.2);
        let (lv, _) = choose(8, 15);
        assert!(lv < 8);
        assert_eq!(verdict(6, 0.0), "solid");
        assert_eq!(verdict(6, 0.5), "fails");
    }

    #[test]
    fn times_are_plausible() {
        let (blocks, seconds) = estimate(32, ONE_WAY_LEVEL, level(ONE_WAY_LEVEL).redundancy, false, false);
        assert_eq!(blocks, 1);
        assert!((10.0..40.0).contains(&seconds), "{seconds}");
        assert_eq!(clock(102.4), "1m 42s");
    }
}
