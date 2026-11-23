//! Reed-Solomon over GF(256), errors-and-erasures decoding.
//!
//! One byte is one symbol. A block with `nsym` parity bytes repairs any mix
//! of e wrong bytes and f bytes flagged as doubtful with 2e + f <= nsym.
//! Codewords are message first, then parity; the byte at index i is the
//! coefficient of x^(n-1-i).

struct Field {
    exp: [u8; 512],
    log: [u8; 256],
}

fn field() -> &'static Field {
    static FIELD: std::sync::OnceLock<Field> = std::sync::OnceLock::new();
    FIELD.get_or_init(|| {
        let mut f = Field { exp: [0; 512], log: [0; 256] };
        let mut x: u16 = 1;
        for i in 0..255 {
            f.exp[i] = x as u8;
            f.log[x as usize] = i as u8;
            x <<= 1;
            if x & 0x100 != 0 {
                x ^= 0x11D;
            }
        }
        for i in 255..512 {
            f.exp[i] = f.exp[i - 255];
        }
        f
    })
}

fn mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        return 0;
    }
    let f = field();
    f.exp[f.log[a as usize] as usize + f.log[b as usize] as usize]
}

fn div(a: u8, b: u8) -> u8 {
    if a == 0 {
        return 0;
    }
    let f = field();
    f.exp[f.log[a as usize] as usize + 255 - f.log[b as usize] as usize]
}

fn alpha(power: usize) -> u8 {
    field().exp[power % 255]
}

/// Polynomial product, coefficients lowest degree first.
fn pmul(p: &[u8], q: &[u8]) -> Vec<u8> {
    let mut r = vec![0u8; p.len() + q.len() - 1];
    for (i, &a) in p.iter().enumerate() {
        if a != 0 {
            for (j, &b) in q.iter().enumerate() {
                r[i + j] ^= mul(a, b);
            }
        }
    }
    r
}

fn peval(p: &[u8], x: u8) -> u8 {
    p.iter().rev().fold(0, |y, &c| mul(y, x) ^ c)
}

/// Generator polynomial, highest degree first, roots alpha^0..alpha^(nsym-1).
fn generator(nsym: usize) -> Vec<u8> {
    let mut g = vec![1u8];
    for k in 0..nsym {
        g = pmul(&g, &[alpha(k), 1]);
    }
    g.reverse();
    g
}

pub fn encode(msg: &[u8], nsym: usize) -> Vec<u8> {
    assert!(msg.len() + nsym <= 255, "block too long");
    let g = generator(nsym);
    let mut rem = msg.to_vec();
    rem.resize(msg.len() + nsym, 0);
    for i in 0..msg.len() {
        let c = rem[i];
        if c != 0 {
            for j in 1..g.len() {
                rem[i + j] ^= mul(g[j], c);
            }
        }
    }
    let mut out = msg.to_vec();
    out.extend_from_slice(&rem[msg.len()..]);
    out
}

fn syndromes(cw: &[u8], nsym: usize) -> Vec<u8> {
    (0..nsym).map(|k| cw.iter().fold(0, |y, &c| mul(y, alpha(k)) ^ c)).collect()
}

/// The corrected message (without parity), or None if it cannot be repaired.
pub fn decode(cw: &[u8], nsym: usize, erasures: &[usize]) -> Option<Vec<u8>> {
    let n = cw.len();
    let mut cw = cw.to_vec();
    let synd = syndromes(&cw, nsym);
    if synd.iter().all(|&s| s == 0) {
        cw.truncate(n - nsym);
        return Some(cw);
    }
    let mut erasures = erasures.to_vec();
    erasures.sort_unstable();
    erasures.dedup();
    if erasures.len() > nsym {
        return None;
    }

    // Erasure locator, then syndromes with the erasures folded in.
    let mut gamma = vec![1u8];
    for &i in &erasures {
        gamma = pmul(&gamma, &[1, alpha(n - 1 - i)]);
    }
    let mut t = pmul(&synd, &gamma);
    t.truncate(nsym);
    let seq = &t[erasures.len()..];

    // Berlekamp-Massey for the error locator.
    let (mut c, mut b) = (vec![1u8], vec![1u8]);
    let (mut big_l, mut m, mut bb) = (0usize, 1usize, 1u8);
    for (i, &s) in seq.iter().enumerate() {
        let mut d = s;
        for j in 1..=big_l {
            if j < c.len() {
                d ^= mul(c[j], seq[i - j]);
            }
        }
        if d == 0 {
            m += 1;
            continue;
        }
        let coef = div(d, bb);
        let mut nc = c.clone();
        if nc.len() < b.len() + m {
            nc.resize(b.len() + m, 0);
        }
        for (j, &v) in b.iter().enumerate() {
            nc[j + m] ^= mul(coef, v);
        }
        if 2 * big_l <= i {
            b = c;
            bb = d;
            big_l = i + 1 - big_l;
            m = 1;
        } else {
            m += 1;
        }
        c = nc;
    }
    if 2 * big_l > seq.len() {
        return None;
    }

    let mut psi = pmul(&c, &gamma);
    while psi.len() > 1 && *psi.last().unwrap() == 0 {
        psi.pop();
    }
    let mut omega = pmul(&synd, &psi);
    omega.truncate(nsym);
    // Formal derivative: in characteristic 2 only odd powers survive.
    let dpsi: Vec<u8> = (1..psi.len()).map(|j| if j & 1 == 1 { psi[j] } else { 0 }).collect();

    // Chien search and Forney's formula.
    let mut found = 0;
    for i in 0..n {
        let p = n - 1 - i;
        let xinv = alpha(255 - p % 255);
        if peval(&psi, xinv) != 0 {
            continue;
        }
        let den = peval(&dpsi, xinv);
        if den == 0 {
            return None;
        }
        cw[i] ^= mul(alpha(p), div(peval(&omega, xinv), den));
        found += 1;
    }
    if found != psi.len() - 1 || syndromes(&cw, nsym).iter().any(|&s| s != 0) {
        return None;
    }
    cw.truncate(n - nsym);
    Some(cw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::Rng;

    #[test]
    fn known_vector() {
        assert_eq!(encode(&[1, 2, 3, 4], 6), [1, 2, 3, 4, 0xcc, 0x0c, 0xc0, 0xf5, 0x6c, 0x9d]);
    }

    #[test]
    fn repairs_errors_and_erasures() {
        let mut rng = Rng::new(1);
        for _ in 0..1500 {
            let nsym = 2 + rng.below(39);
            let k = 1 + rng.below(255 - nsym);
            let msg: Vec<u8> = (0..k).map(|_| rng.below(256) as u8).collect();
            let mut cw = encode(&msg, nsym);
            let n = cw.len();
            let f = rng.below(nsym + 1);
            let e = rng.below((nsym - f) / 2 + 1);
            let mut pos: Vec<usize> = (0..n).collect();
            for i in 0..n.min(e + f) {
                let j = i + rng.below(n - i);
                pos.swap(i, j);
            }
            let hit = &pos[..n.min(e + f)];
            let erased: Vec<usize> = hit.iter().take(f).cloned().collect();
            for &p in hit {
                cw[p] ^= 1 + rng.below(255) as u8;
            }
            assert_eq!(decode(&cw, nsym, &erased).as_deref(), Some(&msg[..]));
        }
    }

    #[test]
    fn refuses_what_it_cannot_repair() {
        let mut rng = Rng::new(2);
        let mut wrong = 0;
        for _ in 0..300 {
            let msg: Vec<u8> = (0..40).map(|_| rng.below(256) as u8).collect();
            let mut cw = encode(&msg, 8);
            for p in [1, 7, 13, 22, 30, 41] {
                cw[p] ^= 1 + rng.below(255) as u8;
            }
            if let Some(out) = decode(&cw, 8, &[]) {
                wrong += (out != msg) as usize;
            }
        }
        assert!(wrong <= 2);
    }
}
