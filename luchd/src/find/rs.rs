//! Reed-Solomon over GF(256) (polynomial 0x11d, first root α^0), the code QR codes and ggwave use. The sound modem
//! puts its frames through it: a few tones lost to a clap or a fan are corrected, not just noticed.

const POLY: u16 = 0x11d;

struct Gf {
    exp: [u8; 512],
    log: [u8; 256],
}

const fn tables() -> Gf {
    let mut exp = [0u8; 512];
    let mut log = [0u8; 256];
    let mut x: u16 = 1;
    let mut i = 0;
    while i < 255 {
        exp[i] = x as u8;
        log[x as usize] = i as u8;
        x <<= 1;
        if x & 0x100 != 0 {
            x ^= POLY;
        }
        i += 1;
    }
    while i < 512 {
        exp[i] = exp[i - 255];
        i += 1;
    }
    Gf { exp, log }
}

static GF: Gf = tables();

fn mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        return 0;
    }
    GF.exp[GF.log[a as usize] as usize + GF.log[b as usize] as usize]
}

fn div(a: u8, b: u8) -> u8 {
    debug_assert!(b != 0);
    if a == 0 {
        return 0;
    }
    GF.exp[(GF.log[a as usize] as usize + 255 - GF.log[b as usize] as usize) % 255]
}

fn pow_alpha(e: usize) -> u8 {
    GF.exp[e % 255]
}

#[cfg_attr(not(test), allow(dead_code))] // only the tests encode (see modem.rs)
/// The generator polynomial (highest degree first): (x - α^0)(x - α^1)...(x - α^(n-1)).
fn generator(nsym: usize) -> Vec<u8> {
    let mut g = vec![1u8];
    for i in 0..nsym {
        let root = pow_alpha(i);
        let mut next = vec![0u8; g.len() + 1];
        for (j, &c) in g.iter().enumerate() {
            next[j] ^= c;
            next[j + 1] ^= mul(c, root);
        }
        g = next;
    }
    g
}

#[cfg_attr(not(test), allow(dead_code))] // only the tests encode (see modem.rs)
/// The message followed by `nsym` parity bytes.
pub fn encode(msg: &[u8], nsym: usize) -> Vec<u8> {
    assert!(
        msg.len() + nsym <= 255,
        "a Reed-Solomon block holds 255 bytes"
    );
    let g = generator(nsym);
    let mut rem = vec![0u8; nsym];
    for &m in msg {
        let factor = m ^ rem[0];
        rem.rotate_left(1);
        rem[nsym - 1] = 0;
        for (r, &gc) in rem.iter_mut().zip(&g[1..]) {
            *r ^= mul(gc, factor);
        }
    }
    let mut out = msg.to_vec();
    out.extend(rem);
    out
}

fn syndromes(word: &[u8], nsym: usize) -> Vec<u8> {
    (0..nsym)
        .map(|j| {
            let a = pow_alpha(j);
            word.iter().fold(0u8, |s, &c| mul(s, a) ^ c)
        })
        .collect()
}

/// Corrects up to `nsym / 2` wrong bytes in place and returns how many there were, or None when there are more.
pub fn correct(word: &mut [u8], nsym: usize) -> Option<usize> {
    let s = syndromes(word, nsym);
    if s.iter().all(|&x| x == 0) {
        return Some(0);
    }
    // Berlekamp-Massey: the error locator Λ, lowest degree first.
    let (mut c, mut b) = (vec![1u8], vec![1u8]);
    let (mut l, mut m, mut bd) = (0usize, 1usize, 1u8);
    for n in 0..nsym {
        let mut d = s[n];
        for i in 1..=l.min(c.len() - 1) {
            d ^= mul(c[i], s[n - i]);
        }
        if d == 0 {
            m += 1;
            continue;
        }
        let coef = div(d, bd);
        let mut next = c.clone();
        if next.len() < b.len() + m {
            next.resize(b.len() + m, 0);
        }
        for (i, &bc) in b.iter().enumerate() {
            next[i + m] ^= mul(coef, bc);
        }
        if 2 * l <= n {
            b = c;
            l = n + 1 - l;
            bd = d;
            m = 1;
        } else {
            m += 1;
        }
        c = next;
    }
    if l == 0 || 2 * l > nsym {
        return None;
    }
    let n = word.len();
    let eval = |p: &[u8], x: u8| p.iter().rev().fold(0u8, |acc, &k| mul(acc, x) ^ k);
    // Chien search: position i is the coefficient of x^(n-1-i); Λ has a root at X^-1 for each wrong one.
    let positions: Vec<usize> = (0..n)
        .filter(|&i| {
            let x_inv = pow_alpha(255 - (n - 1 - i) % 255);
            eval(&c, x_inv) == 0
        })
        .collect();
    if positions.len() != l {
        return None;
    }
    // Forney: Ω = S·Λ mod x^nsym; e = X·Ω(X^-1) / Λ'(X^-1).
    let mut omega = vec![0u8; nsym];
    for (i, &si) in s.iter().enumerate() {
        for (j, &cj) in c.iter().enumerate() {
            if i + j < nsym {
                omega[i + j] ^= mul(si, cj);
            }
        }
    }
    let deriv: Vec<u8> = (1..c.len())
        .map(|i| if i % 2 == 1 { c[i] } else { 0 })
        .collect();
    for &i in &positions {
        let power = (n - 1 - i) % 255;
        let x = pow_alpha(power);
        let x_inv = pow_alpha(255 - power);
        let denom = eval(&deriv, x_inv);
        if denom == 0 {
            return None;
        }
        word[i] ^= mul(x, div(eval(&omega, x_inv), denom));
    }
    // A word too damaged can look correctable: only a clean result counts.
    syndromes(word, nsym)
        .iter()
        .all(|&x| x == 0)
        .then_some(positions.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small, repeatable source of test noise (no dependency for a random crate).
    pub(crate) struct Lcg(pub u64);
    impl Lcg {
        pub fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }
    }

    #[test]
    fn known_codeword() {
        // Shared with the receiver's ChirpTest: both sides must make the very same parity.
        let word = encode(b"Luch", 8);
        assert_eq!(super::super::modem::hex(&word), "4c7563681c7c9b839d89732d");
    }

    #[test]
    fn corrects_up_to_half_the_parity_and_refuses_more() {
        let mut rng = Lcg(7);
        for round in 0..300 {
            let len = 1 + (rng.next() % 40) as usize;
            let msg: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
            let good = encode(&msg, 8);
            let mut bad = good.clone();
            let errors = round % 5; // 0..=4
            let mut hit = std::collections::HashSet::new();
            while hit.len() < errors {
                hit.insert((rng.next() as usize) % bad.len());
            }
            for &i in &hit {
                bad[i] ^= 1 + (rng.next() % 255) as u8;
            }
            assert_eq!(correct(&mut bad, 8), Some(errors), "round {round}");
            assert_eq!(bad, good);
        }
        // Six wrong bytes are beyond it: it must say so (or, rarely, land on another valid word), never panic.
        let mut refused = 0;
        for _ in 0..200 {
            let msg: Vec<u8> = (0..16).map(|_| rng.next() as u8).collect();
            let good = encode(&msg, 8);
            let mut bad = good.clone();
            for i in 0..6 {
                bad[i * 3] ^= 0x5a;
            }
            if correct(&mut bad, 8).is_none() {
                refused += 1;
            } else {
                assert_ne!(bad, good);
            }
        }
        assert!(refused > 190, "refused {refused} of 200");
    }
}
