//! Shared-key encryption: ChaCha20-Poly1305 (RFC 8439) with the key derived
//! from a passphrase by PBKDF2-HMAC-SHA256 (RFC 8018).
//!
//! These are plain implementations of the published algorithms, checked
//! against the RFC test vectors below. They have not been independently
//! audited, and make no attempt to resist timing attacks from someone
//! sharing the machine.

pub const SALT_LEN: usize = 8;
pub const NONCE_LEN: usize = 4;
pub const TAG_LEN: usize = 16;
/// Bytes added to each encrypted block: salt, counter and authentication tag.
pub const OVERHEAD: usize = SALT_LEN + NONCE_LEN + TAG_LEN;
const ITERATIONS: u32 = 200_000;

// ------------------------------------------------------------------ SHA-256

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

#[derive(Clone)]
struct Sha256 {
    state: [u32; 8],
    buffer: Vec<u8>,
    length: u64,
}

impl Sha256 {
    fn new() -> Sha256 {
        Sha256 {
            state: [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19],
            buffer: Vec::with_capacity(64),
            length: 0,
        }
    }

    fn compress(&mut self, block: &[u8]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([block[4 * i], block[4 * i + 1], block[4 * i + 2], block[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (s, v) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *s = s.wrapping_add(v);
        }
    }

    fn update(&mut self, data: &[u8]) {
        self.length += data.len() as u64;
        self.buffer.extend_from_slice(data);
        let whole = self.buffer.len() / 64 * 64;
        let pending: Vec<u8> = self.buffer.drain(..whole).collect();
        for block in pending.chunks(64) {
            self.compress(block);
        }
    }

    fn finish(mut self) -> [u8; 32] {
        let bits = self.length * 8;
        let mut tail = std::mem::take(&mut self.buffer);
        tail.push(0x80);
        while tail.len() % 64 != 56 {
            tail.push(0);
        }
        tail.extend_from_slice(&bits.to_be_bytes());
        for block in tail.chunks(64) {
            self.compress(block);
        }
        let mut out = [0u8; 32];
        for (i, s) in self.state.iter().enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&s.to_be_bytes());
        }
        out
    }
}

#[cfg(test)]
fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    h.finish()
}

/// HMAC-SHA256 with the key schedule done once, for the many calls PBKDF2 makes.
struct Hmac {
    inner: Sha256,
    outer: Sha256,
}

impl Hmac {
    fn new(key: &[u8]) -> Hmac {
        let mut block = [0u8; 64];
        if key.len() > 64 {
            let mut h = Sha256::new();
            h.update(key);
            block[..32].copy_from_slice(&h.finish());
        } else {
            block[..key.len()].copy_from_slice(key);
        }
        let (mut inner, mut outer) = (Sha256::new(), Sha256::new());
        inner.update(&block.map(|b| b ^ 0x36));
        outer.update(&block.map(|b| b ^ 0x5c));
        Hmac { inner, outer }
    }

    fn mac(&self, data: &[u8]) -> [u8; 32] {
        let mut inner = self.inner.clone();
        inner.update(data);
        let mut outer = self.outer.clone();
        outer.update(&inner.finish());
        outer.finish()
    }
}

fn pbkdf2(password: &[u8], salt: &[u8], iterations: u32, out: &mut [u8]) {
    let hmac = Hmac::new(password);
    for (i, chunk) in out.chunks_mut(32).enumerate() {
        let mut message = salt.to_vec();
        message.extend_from_slice(&(i as u32 + 1).to_be_bytes());
        let mut u = hmac.mac(&message);
        let mut t = u;
        for _ in 1..iterations {
            u = hmac.mac(&u);
            for (a, b) in t.iter_mut().zip(u) {
                *a ^= b;
            }
        }
        chunk.copy_from_slice(&t[..chunk.len()]);
    }
}

// ----------------------------------------------------------------- ChaCha20

fn chacha_block(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u8; 64] {
    let mut s = [0u32; 16];
    s[..4].copy_from_slice(&[0x61707865, 0x3320646e, 0x79622d32, 0x6b206574]);
    for i in 0..8 {
        s[4 + i] = u32::from_le_bytes([key[4 * i], key[4 * i + 1], key[4 * i + 2], key[4 * i + 3]]);
    }
    s[12] = counter;
    for i in 0..3 {
        s[13 + i] = u32::from_le_bytes([nonce[4 * i], nonce[4 * i + 1], nonce[4 * i + 2], nonce[4 * i + 3]]);
    }
    let mut w = s;
    fn quarter(w: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
        w[a] = w[a].wrapping_add(w[b]);
        w[d] = (w[d] ^ w[a]).rotate_left(16);
        w[c] = w[c].wrapping_add(w[d]);
        w[b] = (w[b] ^ w[c]).rotate_left(12);
        w[a] = w[a].wrapping_add(w[b]);
        w[d] = (w[d] ^ w[a]).rotate_left(8);
        w[c] = w[c].wrapping_add(w[d]);
        w[b] = (w[b] ^ w[c]).rotate_left(7);
    }
    for _ in 0..10 {
        quarter(&mut w, 0, 4, 8, 12);
        quarter(&mut w, 1, 5, 9, 13);
        quarter(&mut w, 2, 6, 10, 14);
        quarter(&mut w, 3, 7, 11, 15);
        quarter(&mut w, 0, 5, 10, 15);
        quarter(&mut w, 1, 6, 11, 12);
        quarter(&mut w, 2, 7, 8, 13);
        quarter(&mut w, 3, 4, 9, 14);
    }
    let mut out = [0u8; 64];
    for i in 0..16 {
        out[4 * i..4 * i + 4].copy_from_slice(&w[i].wrapping_add(s[i]).to_le_bytes());
    }
    out
}

fn chacha_xor(key: &[u8; 32], nonce: &[u8; 12], data: &mut [u8]) {
    for (i, chunk) in data.chunks_mut(64).enumerate() {
        let stream = chacha_block(key, i as u32 + 1, nonce);
        for (d, s) in chunk.iter_mut().zip(stream) {
            *d ^= s;
        }
    }
}

// ----------------------------------------------------------------- Poly1305

/// Poly1305 of a message whose length is a multiple of 16, which is all the
/// AEAD construction ever feeds it.
fn poly1305(key: &[u8; 32], message: &[u8]) -> [u8; 16] {
    debug_assert!(message.len() % 16 == 0);
    let le = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64;
    const MASK: u64 = 0x3ff_ffff;
    let r0 = le(&key[0..]) & 0x3ff_ffff;
    let r1 = (le(&key[3..]) >> 2) & 0x3ff_ff03;
    let r2 = (le(&key[6..]) >> 4) & 0x3ff_c0ff;
    let r3 = (le(&key[9..]) >> 6) & 0x3f0_3fff;
    let r4 = (le(&key[12..]) >> 8) & 0x00f_ffff;
    let (s1, s2, s3, s4) = (r1 * 5, r2 * 5, r3 * 5, r4 * 5);
    let (mut h0, mut h1, mut h2, mut h3, mut h4) = (0u64, 0u64, 0u64, 0u64, 0u64);
    for block in message.chunks(16) {
        h0 += le(&block[0..]) & MASK;
        h1 += (le(&block[3..]) >> 2) & MASK;
        h2 += (le(&block[6..]) >> 4) & MASK;
        h3 += (le(&block[9..]) >> 6) & MASK;
        h4 += (le(&block[12..]) >> 8) | (1 << 24);
        let d0 = h0 * r0 + h1 * s4 + h2 * s3 + h3 * s2 + h4 * s1;
        let mut d1 = h0 * r1 + h1 * r0 + h2 * s4 + h3 * s3 + h4 * s2;
        let mut d2 = h0 * r2 + h1 * r1 + h2 * r0 + h3 * s4 + h4 * s3;
        let mut d3 = h0 * r3 + h1 * r2 + h2 * r1 + h3 * r0 + h4 * s4;
        let mut d4 = h0 * r4 + h1 * r3 + h2 * r2 + h3 * r1 + h4 * r0;
        let mut c = d0 >> 26;
        h0 = d0 & MASK;
        d1 += c;
        c = d1 >> 26;
        h1 = d1 & MASK;
        d2 += c;
        c = d2 >> 26;
        h2 = d2 & MASK;
        d3 += c;
        c = d3 >> 26;
        h3 = d3 & MASK;
        d4 += c;
        c = d4 >> 26;
        h4 = d4 & MASK;
        h0 += c * 5;
        c = h0 >> 26;
        h0 &= MASK;
        h1 += c;
    }
    // Full carry, then subtract the modulus 2^130 - 5 if the value reaches it.
    let mut c = h1 >> 26;
    h1 &= MASK;
    h2 += c;
    c = h2 >> 26;
    h2 &= MASK;
    h3 += c;
    c = h3 >> 26;
    h3 &= MASK;
    h4 += c;
    c = h4 >> 26;
    h4 &= MASK;
    h0 += c * 5;
    c = h0 >> 26;
    h0 &= MASK;
    h1 += c;
    let mut g0 = h0 + 5;
    c = g0 >> 26;
    g0 &= MASK;
    let mut g1 = h1 + c;
    c = g1 >> 26;
    g1 &= MASK;
    let mut g2 = h2 + c;
    c = g2 >> 26;
    g2 &= MASK;
    let mut g3 = h3 + c;
    c = g3 >> 26;
    g3 &= MASK;
    let g4 = h4 + c;
    if g4 >> 26 != 0 {
        (h0, h1, h2, h3, h4) = (g0, g1, g2, g3, g4 & MASK);
    }
    let value = (h0 as u128) | (h1 as u128) << 26 | (h2 as u128) << 52 | (h3 as u128) << 78 | (h4 as u128) << 104;
    let pad = u128::from_le_bytes(key[16..32].try_into().unwrap());
    value.wrapping_add(pad).to_le_bytes()
}

fn tag(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], ciphertext: &[u8]) -> [u8; 16] {
    let block = chacha_block(key, 0, nonce);
    let poly_key: [u8; 32] = block[..32].try_into().unwrap();
    let mut message = aad.to_vec();
    message.resize(aad.len().div_ceil(16) * 16, 0);
    message.extend_from_slice(ciphertext);
    message.resize(message.len().div_ceil(16) * 16, 0);
    message.extend_from_slice(&(aad.len() as u64).to_le_bytes());
    message.extend_from_slice(&(ciphertext.len() as u64).to_le_bytes());
    poly1305(&poly_key, &message)
}

// -------------------------------------------------------------- randomness

#[cfg(unix)]
fn random(buf: &mut [u8]) -> std::io::Result<()> {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")?.read_exact(buf)
}

#[cfg(windows)]
fn random(buf: &mut [u8]) -> std::io::Result<()> {
    #[link(name = "advapi32")]
    extern "system" {
        // RtlGenRandom, the system's cryptographic random source.
        fn SystemFunction036(buffer: *mut u8, length: u32) -> u8;
    }
    if unsafe { SystemFunction036(buf.as_mut_ptr(), buf.len() as u32) } != 0 {
        Ok(())
    } else {
        Err(std::io::Error::other("the system random source failed"))
    }
}

#[cfg(not(any(unix, windows)))]
fn random(_buf: &mut [u8]) -> std::io::Result<()> {
    Err(std::io::Error::other("no secure random source on this platform"))
}

// --------------------------------------------------------------- the cipher

/// Encrypts and decrypts blocks under one passphrase.
///
/// Each block on the air is `salt | counter | ciphertext | tag`. The key is
/// derived from the passphrase and the salt; a sender picks one random salt
/// per run and never reuses a counter with it. A receiver refuses a counter
/// it has already seen under the same salt, so a recording cannot be replayed
/// into the same session.
pub struct Cipher {
    passphrase: Vec<u8>,
    salt: [u8; SALT_LEN],
    key: Option<([u8; SALT_LEN], [u8; 32])>,
    counter: u32,
    seen: Option<([u8; SALT_LEN], u32)>,
}

impl Cipher {
    pub fn new(passphrase: &str) -> std::io::Result<Cipher> {
        let mut salt = [0u8; SALT_LEN];
        random(&mut salt)?;
        Ok(Cipher { passphrase: passphrase.as_bytes().to_vec(), salt, key: None, counter: 0, seen: None })
    }

    fn key_for(&mut self, salt: [u8; SALT_LEN]) -> [u8; 32] {
        if let Some((s, k)) = self.key {
            if s == salt {
                return k;
            }
        }
        let mut key = [0u8; 32];
        let mut salted = b"vocsend v1 ".to_vec();
        salted.extend_from_slice(&salt);
        pbkdf2(&self.passphrase, &salted, ITERATIONS, &mut key);
        self.key = Some((salt, key));
        key
    }

    fn nonce(counter: u32) -> [u8; 12] {
        let mut nonce = [0u8; 12];
        nonce[8..].copy_from_slice(&counter.to_be_bytes());
        nonce
    }

    /// `aad` is bound to the block but not sent: both sides must supply the same.
    pub fn seal(&mut self, plaintext: &[u8], aad: &[u8]) -> Vec<u8> {
        self.counter += 1;
        let key = self.key_for(self.salt);
        let nonce = Self::nonce(self.counter);
        let mut body = plaintext.to_vec();
        chacha_xor(&key, &nonce, &mut body);
        let mut out = self.salt.to_vec();
        out.extend_from_slice(&self.counter.to_be_bytes());
        out.extend_from_slice(&body);
        out.extend_from_slice(&tag(&key, &nonce, aad, &body));
        out
    }

    pub fn open(&mut self, block: &[u8], aad: &[u8]) -> Result<Vec<u8>, &'static str> {
        const WRONG: &str = "wrong key, or the data was altered";
        if block.len() < OVERHEAD {
            return Err(WRONG);
        }
        let salt: [u8; SALT_LEN] = block[..SALT_LEN].try_into().unwrap();
        let counter = u32::from_be_bytes(block[SALT_LEN..SALT_LEN + NONCE_LEN].try_into().unwrap());
        let (body, theirs) = block[SALT_LEN + NONCE_LEN..].split_at(block.len() - OVERHEAD);
        let key = self.key_for(salt);
        let nonce = Self::nonce(counter);
        let ours = tag(&key, &nonce, aad, body);
        if ours.iter().zip(theirs).fold(0u8, |d, (a, b)| d | (a ^ b)) != 0 {
            return Err(WRONG);
        }
        if let Some((s, last)) = self.seen {
            if s == salt && counter <= last {
                return Err("this block was already received once: refusing a replay");
            }
        }
        self.seen = Some((salt, counter));
        let mut out = body.to_vec();
        chacha_xor(&key, &nonce, &mut out);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn sha256_vectors() {
        assert_eq!(sha256(b"abc").to_vec(), hex("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"));
        assert_eq!(sha256(b"").to_vec(), hex("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"));
        let long = vec![b'a'; 1000];
        let mut h = Sha256::new();
        for chunk in long.chunks(7) {
            h.update(chunk);
        }
        assert_eq!(h.finish(), sha256(&long));
    }

    #[test]
    fn hmac_rfc4231_case_2() {
        let mac = Hmac::new(b"Jefe").mac(b"what do ya want for nothing?");
        assert_eq!(mac.to_vec(), hex("5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"));
    }

    #[test]
    fn pbkdf2_rfc7914() {
        let mut out = [0u8; 64];
        pbkdf2(b"passwd", b"salt", 1, &mut out);
        assert_eq!(out.to_vec(), hex(
            "55ac046e56e3089fec1691c22544b605f94185216dde0465e68b9d57c20dacbc\
             49ca9cccf179b645991664b39d77ef317c71b845b1e30bd509112041d3a19783"));
    }

    #[test]
    fn chacha20_poly1305_rfc8439() {
        let key: [u8; 32] = hex("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f").try_into().unwrap();
        let nonce: [u8; 12] = hex("070000004041424344454647").try_into().unwrap();
        let aad = hex("50515253c0c1c2c3c4c5c6c7");
        let mut text = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.".to_vec();
        chacha_xor(&key, &nonce, &mut text);
        assert_eq!(text[..16].to_vec(), hex("d31a8d34648e60db7b86afbc53ef7ec2"));
        assert_eq!(tag(&key, &nonce, &aad, &text).to_vec(), hex("1ae10b594f09e26a7e902ecbd0600691"));
    }

    #[test]
    fn poly1305_rfc8439_section_2_5() {
        // The standalone Poly1305 example, padded to a whole block as the AEAD does:
        // checked through the AEAD vector above; here just that the tag depends on every input.
        let key = [7u8; 32];
        let a = poly1305(&key, &[1u8; 32]);
        let mut changed = [1u8; 32];
        changed[31] ^= 1;
        assert_ne!(a, poly1305(&key, &changed));
    }

    #[test]
    fn seal_and_open() {
        let mut tx = Cipher::new("correct horse").unwrap();
        let mut rx = Cipher::new("correct horse").unwrap();
        let sealed = tx.seal(b"attack at dawn", b"aad");
        assert_eq!(sealed.len(), 14 + OVERHEAD);
        assert_eq!(rx.open(&sealed, b"aad").unwrap(), b"attack at dawn");
        assert!(rx.open(&sealed, b"aad").is_err(), "replay must be refused");
        let second = tx.seal(b"", b"aad");
        assert_eq!(rx.open(&second, b"aad").unwrap(), b"");
        let third = tx.seal(b"x", b"aad");
        assert!(rx.open(&third, b"other").is_err(), "aad is bound");
        let mut tampered = tx.seal(b"hello", b"");
        tampered[SALT_LEN + NONCE_LEN] ^= 1;
        assert!(rx.open(&tampered, b"").is_err());
        let mut wrong = Cipher::new("battery staple").unwrap();
        assert!(wrong.open(&tx.seal(b"hello", b""), b"").is_err());
    }
}
