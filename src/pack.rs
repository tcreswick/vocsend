//! Compression: DEFLATE, from the flate2 library.

use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use flate2::Compression;
use std::io::{Read, Write};

/// The most a transfer may expand to: far above anything this tool is for.
const LIMIT: u64 = 64 << 20;

pub fn compress(data: &[u8]) -> Vec<u8> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(data).expect("writing to memory cannot fail");
    encoder.finish().expect("writing to memory cannot fail")
}

pub fn decompress(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    DeflateDecoder::new(data).take(LIMIT + 1).read_to_end(&mut out).ok()?;
    (out.len() as u64 <= LIMIT).then_some(out)
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
        }
    }

    #[test]
    fn shrinks_text_and_base64() {
        let mut rng = Rng::new(11);
        let base64: Vec<u8> = (0..1200)
            .map(|_| b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"[rng.below(64)])
            .collect();
        let prose = b"The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog again, \
                      and then the lazy dog jumps over the quick brown fox, which nobody expected at all.";
        assert!(compress(&base64).len() < base64.len() * 85 / 100, "{}", compress(&base64).len());
        assert!(compress(prose).len() < prose.len() * 75 / 100, "{}", compress(prose).len());
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
