//! Framing: a block of bytes <-> the bytes that are actually sent.
//!
//!   header  4 bytes [version, length hi, length lo, parity per block]
//!           + 6 Reed-Solomon parity bytes
//!   body    data + CRC32, split into Reed-Solomon blocks, interleaved so
//!           that a burst of noise is spread across the blocks

use crate::rs;

pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 10;
const HEADER_PARITY: usize = 6;
pub const MAX_DATA: usize = 0xFFFF - 4;

pub fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *slot = c;
        }
        t
    });
    !data.iter().fold(!0u32, |c, &b| table[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8))
}

/// Data bytes per Reed-Solomon block for a body of `length` bytes.
fn layout(length: usize, nsym: usize) -> Vec<usize> {
    let blocks = length.div_ceil(255 - nsym).max(1);
    (0..blocks).map(|i| length / blocks + (i < length % blocks) as usize).collect()
}

/// Transmission order as (block, index within block).
fn order(length: usize, nsym: usize) -> Vec<(usize, usize)> {
    let sizes: Vec<usize> = layout(length, nsym).iter().map(|k| k + nsym).collect();
    let longest = *sizes.iter().max().unwrap();
    let mut out = Vec::new();
    for i in 0..longest {
        for (b, &n) in sizes.iter().enumerate() {
            if i < n {
                out.push((b, i));
            }
        }
    }
    out
}

/// Bytes on the air after the header for a body of `length` bytes.
pub fn body_len(length: usize, nsym: usize) -> usize {
    length + nsym * layout(length, nsym).len()
}

/// Parity bytes per block for a given amount of redundancy.
fn parity(length: usize, redundancy: f64) -> usize {
    let per_block = (length as f64).min(255.0 / (1.0 + redundancy));
    ((redundancy * per_block).ceil() as usize).clamp(4, 127)
}

/// Total bytes on the air for `data_len` bytes of data.
pub fn encoded_len(data_len: usize, redundancy: f64) -> usize {
    let length = data_len + 4;
    HEADER_LEN + body_len(length, parity(length, redundancy))
}

pub fn encode(data: &[u8], redundancy: f64) -> Vec<u8> {
    assert!(data.len() <= MAX_DATA);
    let mut body = data.to_vec();
    body.extend_from_slice(&crc32(data).to_be_bytes());
    let nsym = parity(body.len(), redundancy);
    let mut out = rs::encode(&[VERSION << 4, (body.len() >> 8) as u8, body.len() as u8, nsym as u8], HEADER_PARITY);
    let mut blocks = Vec::new();
    let mut at = 0;
    for k in layout(body.len(), nsym) {
        blocks.push(rs::encode(&body[at..at + k], nsym));
        at += k;
    }
    out.extend(order(body.len(), nsym).iter().map(|&(b, i)| blocks[b][i]));
    out
}

/// Reed-Solomon decode, progressively treating the least certain bytes as erasures.
fn correct(cw: &[u8], nsym: usize, confidence: &[f32]) -> Option<Vec<u8>> {
    let mut ranked: Vec<usize> = (0..cw.len()).collect();
    ranked.sort_by(|&a, &b| confidence[a].partial_cmp(&confidence[b]).unwrap_or(std::cmp::Ordering::Equal));
    (0..=nsym).step_by(2).find_map(|erase| rs::decode(cw, nsym, &ranked[..erase.min(ranked.len())]))
}

/// (body length, parity per block) from the first HEADER_LEN bytes.
pub fn decode_header(cw: &[u8], confidence: &[f32]) -> Option<(usize, usize)> {
    let h = correct(&cw[..HEADER_LEN], HEADER_PARITY, &confidence[..HEADER_LEN])?;
    let (length, nsym) = ((h[1] as usize) << 8 | h[2] as usize, h[3] as usize);
    (h[0] >> 4 == VERSION && (2..=127).contains(&nsym) && length >= 4).then_some((length, nsym))
}

pub fn decode_body(stream: &[u8], confidence: &[f32], length: usize, nsym: usize) -> Option<Vec<u8>> {
    let sizes: Vec<usize> = layout(length, nsym).iter().map(|k| k + nsym).collect();
    let mut blocks: Vec<Vec<u8>> = sizes.iter().map(|&n| vec![0; n]).collect();
    let mut conf: Vec<Vec<f32>> = sizes.iter().map(|&n| vec![0.0; n]).collect();
    for (k, &(b, i)) in order(length, nsym).iter().enumerate() {
        blocks[b][i] = *stream.get(k)?;
        conf[b][i] = confidence[k];
    }
    let mut body = Vec::with_capacity(length);
    for (block, c) in blocks.iter().zip(&conf) {
        body.extend(correct(block, nsym, c)?);
    }
    let (data, check) = body.split_at(body.len() - 4);
    (crc32(data).to_be_bytes() == check).then(|| data.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::Rng;

    #[test]
    fn crc_matches_the_standard() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn round_trips_and_repairs() {
        let mut rng = Rng::new(3);
        for &n in &[0usize, 1, 17, 200, 251, 252, 700, 3000] {
            for &red in &[0.15, 0.4, 1.0] {
                let data: Vec<u8> = (0..n).map(|_| rng.below(256) as u8).collect();
                let mut air = encode(&data, red);
                assert_eq!(air.len(), encoded_len(n, red));
                // Damage about a twentieth of the bytes after the header.
                let mut conf = vec![1.0f32; air.len()];
                for i in (HEADER_LEN..air.len()).step_by(23) {
                    air[i] ^= 0x5A;
                    conf[i] = 0.01;
                }
                let (length, nsym) = decode_header(&air, &conf).unwrap();
                assert_eq!(length, n + 4);
                let out = decode_body(&air[HEADER_LEN..], &conf[HEADER_LEN..], length, nsym).unwrap();
                assert_eq!(out, data);
            }
        }
    }
}
