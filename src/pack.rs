//! Compression for small payloads: an adaptive range coder that predicts
//! each byte from the one before it.
//!
//! General-purpose compressors need a few kilobytes to get going. This one
//! is aimed at what actually gets sent: a few hundred bytes of base64 (keys,
//! certificates) or text. It learns as it goes, on both sides identically,
//! so nothing but the coded bytes is sent.

const TOP: u32 = 1 << 24;
const BOTTOM: u32 = 1 << 16;
const CONTEXT_WEIGHT: u32 = 24; // how much the previous byte's statistics outweigh the overall ones
const OVERALL_STEP: u16 = 8; // how fast the overall statistics move away from "anything is possible"

struct Model {
    overall: [u16; 256],
    overall_total: u32,
    after: Vec<[u16; 256]>, // counts of each byte following a given byte
    after_total: [u32; 256],
    previous: usize,
}

impl Model {
    fn new() -> Model {
        Model { overall: [1; 256], overall_total: 256, after: vec![[0; 256]; 256], after_total: [0; 256], previous: 0 }
    }

    fn frequency(&self, symbol: usize) -> u32 {
        CONTEXT_WEIGHT * self.after[self.previous][symbol] as u32 + self.overall[symbol] as u32
    }

    fn total(&self) -> u32 {
        CONTEXT_WEIGHT * self.after_total[self.previous] + self.overall_total
    }

    fn update(&mut self, symbol: usize) {
        let row = &mut self.after[self.previous];
        row[symbol] += 1;
        self.after_total[self.previous] += 1;
        if self.after_total[self.previous] > 1000 {
            self.after_total[self.previous] = row.iter_mut().map(|c| { *c /= 2; *c as u32 }).sum();
        }
        self.overall[symbol] += OVERALL_STEP;
        self.overall_total += OVERALL_STEP as u32;
        if self.overall_total > 8000 {
            self.overall_total = self.overall.iter_mut().map(|c| { *c = (*c / 2).max(1); *c as u32 }).sum();
        }
        self.previous = symbol;
    }
}

fn varint(mut n: usize, out: &mut Vec<u8>) {
    while n >= 0x80 {
        out.push((n as u8 & 0x7f) | 0x80);
        n >>= 7;
    }
    out.push(n as u8);
}

pub fn compress(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    varint(data.len(), &mut out);
    let mut model = Model::new();
    let (mut low, mut range) = (0u32, u32::MAX);
    for &byte in data {
        let symbol = byte as usize;
        let cumulative: u32 = (0..symbol).map(|s| model.frequency(s)).sum();
        let step = range / model.total();
        low = low.wrapping_add(step * cumulative);
        range = step * model.frequency(symbol);
        loop {
            if (low ^ low.wrapping_add(range)) >= TOP {
                if range >= BOTTOM {
                    break;
                }
                range = low.wrapping_neg() & (BOTTOM - 1);
            }
            out.push((low >> 24) as u8);
            low <<= 8;
            range <<= 8;
        }
        model.update(symbol);
    }
    out.extend_from_slice(&low.to_be_bytes());
    out
}

pub fn decompress(data: &[u8]) -> Option<Vec<u8>> {
    let mut at = 0;
    let (mut length, mut shift) = (0usize, 0);
    loop {
        let b = *data.get(at)?;
        at += 1;
        length |= ((b & 0x7f) as usize) << shift;
        if b & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift > 28 {
            return None;
        }
    }
    // A well-formed stream never expands data by more than a small factor.
    if length > data.len().saturating_mul(1000) + 1000 {
        return None;
    }
    let mut next = || {
        let b = data.get(at).copied().unwrap_or(0);
        at += 1;
        b as u32
    };
    let mut code = 0u32;
    for _ in 0..4 {
        code = code << 8 | next();
    }
    let mut model = Model::new();
    let (mut low, mut range) = (0u32, u32::MAX);
    let mut out = Vec::with_capacity(length);
    for _ in 0..length {
        let total = model.total();
        let step = range / total;
        let target = (code.wrapping_sub(low) / step).min(total - 1);
        let (mut symbol, mut cumulative) = (0usize, 0u32);
        loop {
            let f = model.frequency(symbol);
            if cumulative + f > target {
                break;
            }
            cumulative += f;
            symbol += 1;
            if symbol == 256 {
                return None;
            }
        }
        low = low.wrapping_add(step * cumulative);
        range = step * model.frequency(symbol);
        loop {
            if (low ^ low.wrapping_add(range)) >= TOP {
                if range >= BOTTOM {
                    break;
                }
                range = low.wrapping_neg() & (BOTTOM - 1);
            }
            code = code << 8 | next();
            low <<= 8;
            range <<= 8;
        }
        model.update(symbol);
        out.push(symbol as u8);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::Rng;

    #[test]
    fn round_trips() {
        let mut rng = Rng::new(5);
        for n in [0usize, 1, 2, 50, 300, 5000, 70000] {
            let random: Vec<u8> = (0..n).map(|_| rng.below(256) as u8).collect();
            assert_eq!(decompress(&compress(&random)).unwrap(), random);
            let skewed: Vec<u8> = (0..n).map(|_| b"aaaaab cd\n"[rng.below(10)]).collect();
            assert_eq!(decompress(&compress(&skewed)).unwrap(), skewed);
            let runs: Vec<u8> = (0..n).map(|i| (i / 97) as u8).collect();
            assert_eq!(decompress(&compress(&runs)).unwrap(), runs);
        }
    }

    #[test]
    fn shrinks_what_it_is_for() {
        let base64 = b"AAAAC3NzaC1lZDI1NTE5AAAAIHk3Qm5vVGhpc0lzTm90QVJlYWxLZXlCdXRMb29rc0xpa2VPbmVXaXRoQmFzZTY0Q2hhcnMK\
                       MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAu1SU1LfVLPHCozMxH2Mo4lgOEePzNm0tRgeLezV6ffAt0gunVTLw7onLRnrq0";
        let prose = b"The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog again, \
                      and then the lazy dog jumps over the quick brown fox, which nobody expected at all.";
        assert!(compress(base64).len() < base64.len() * 95 / 100, "{} of {}", compress(base64).len(), base64.len());
        assert!(compress(prose).len() < prose.len() * 75 / 100, "{} of {}", compress(prose).len(), prose.len());
    }

    #[test]
    fn garbage_does_not_panic() {
        let mut rng = Rng::new(9);
        for n in 0..200 {
            let junk: Vec<u8> = (0..n).map(|_| rng.below(256) as u8).collect();
            let _ = decompress(&junk);
        }
    }
}
