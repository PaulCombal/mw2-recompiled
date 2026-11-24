// The two primitives a start needs: AES-128 to decrypt an XEX, and SHA-256 to tell
// the disc this build was made from from any other.

pub type Key = [u8; 16];

// AES-128 decryption (FIPS 197, the equivalent inverse cipher written out round by
// round). Only an XEX's 5 MB goes through it, once per launch, so it is table-free
// apart from the S-boxes, which are derived rather than typed.
struct Boxes {
    sbox: [u8; 256],
    inverse: [u8; 256],
}

const BOXES: Boxes = {
    let mut sbox = [0u8; 256];
    // The S-box is the multiplicative inverse in GF(2^8) followed by an affine map;
    // walking p and q = 1/p together yields both at once.
    let (mut p, mut q) = (1u8, 1u8);
    loop {
        p ^= (p << 1) ^ if p & 0x80 != 0 { 0x1B } else { 0 }; // p *= 3
        q ^= q << 1; // q /= 3
        q ^= q << 2;
        q ^= q << 4;
        if q & 0x80 != 0 {
            q ^= 0x09;
        }
        sbox[p as usize] = q ^ q.rotate_left(1) ^ q.rotate_left(2) ^ q.rotate_left(3) ^ q.rotate_left(4) ^ 0x63;
        if p == 1 {
            break;
        }
    }
    sbox[0] = 0x63;
    let mut inverse = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        inverse[sbox[i] as usize] = i as u8;
        i += 1;
    }
    Boxes { sbox, inverse }
};

fn times_2(x: u8) -> u8 {
    (x << 1) ^ if x & 0x80 != 0 { 0x1B } else { 0 }
}

fn multiply(mut x: u8, mut y: u8) -> u8 {
    let mut r = 0;
    while y != 0 {
        if y & 1 != 0 {
            r ^= x;
        }
        y >>= 1;
        x = times_2(x);
    }
    r
}

// The eleven round keys, 16 bytes each.
fn expand(key: &Key) -> [u8; 176] {
    let mut out = [0u8; 176];
    out[..16].copy_from_slice(key);
    let mut rcon = 1u8;
    for i in (16..176).step_by(4) {
        let mut t = [out[i - 4], out[i - 3], out[i - 2], out[i - 1]];
        if i % 16 == 0 {
            t = [
                BOXES.sbox[t[1] as usize] ^ rcon,
                BOXES.sbox[t[2] as usize],
                BOXES.sbox[t[3] as usize],
                BOXES.sbox[t[0] as usize],
            ];
            rcon = times_2(rcon);
        }
        for j in 0..4 {
            out[i + j] = out[i - 16 + j] ^ t[j];
        }
    }
    out
}

fn decrypt_block(keys: &[u8; 176], s: &mut [u8]) {
    for i in 0..16 {
        s[i] ^= keys[160 + i];
    }
    for round in (0..=9).rev() {
        // Inverse ShiftRows: row r (bytes r, r+4, r+8, r+12) rotates right by r.
        let mut t = [0u8; 16];
        for c in 0..4 {
            for r in 0..4 {
                t[((c + r) % 4) * 4 + r] = s[c * 4 + r];
            }
        }
        for i in 0..16 {
            s[i] = BOXES.inverse[t[i] as usize] ^ keys[round * 16 + i];
        }
        if round == 0 {
            break;
        }
        for column in s.chunks_exact_mut(4) {
            // inverse MixColumns
            let [a0, a1, a2, a3] = [column[0], column[1], column[2], column[3]];
            column[0] = multiply(a0, 14) ^ multiply(a1, 11) ^ multiply(a2, 13) ^ multiply(a3, 9);
            column[1] = multiply(a0, 9) ^ multiply(a1, 14) ^ multiply(a2, 11) ^ multiply(a3, 13);
            column[2] = multiply(a0, 13) ^ multiply(a1, 9) ^ multiply(a2, 14) ^ multiply(a3, 11);
            column[3] = multiply(a0, 11) ^ multiply(a1, 13) ^ multiply(a2, 9) ^ multiply(a3, 14);
        }
    }
}

/// CBC with a zero IV, in place, over whole blocks.
pub fn aes128_cbc_decrypt(key: &Key, data: &mut [u8]) {
    let keys = expand(key);
    let mut previous = [0u8; 16];
    for block in data.chunks_exact_mut(16) {
        let mut cipher = [0u8; 16];
        cipher.copy_from_slice(block);
        decrypt_block(&keys, block);
        for i in 0..16 {
            block[i] ^= previous[i];
        }
        previous = cipher;
    }
}

// SHA-256 (FIPS 180-4).
const ROUNDS: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
    0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
    0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
    0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
    0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
    0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
    0xc67178f2,
];

fn block(h: &mut [u32; 8], block: &[u8]) {
    let mut w = [0u32; 64];
    for (word, bytes) in w.iter_mut().zip(block.chunks_exact(4)) {
        *word = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = *h;
    for i in 0..64 {
        let t1 = hh
            .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
            .wrapping_add((e & f) ^ (!e & g))
            .wrapping_add(ROUNDS[i])
            .wrapping_add(w[i]);
        let t2 =
            (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22)).wrapping_add((a & b) ^ (a & c) ^ (b & c));
        hh = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    for (state, value) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
        *state = state.wrapping_add(value);
    }
}

/// Lower-case hex, as CMake's file(SHA256) writes it.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut h = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut blocks = data.chunks_exact(64);
    for whole in blocks.by_ref() {
        block(&mut h, whole);
    }
    let rest = blocks.remainder();
    let mut last = [0u8; 128];
    last[..rest.len()].copy_from_slice(rest);
    last[rest.len()] = 0x80;
    let size = if rest.len() < 56 { 64 } else { 128 };
    last[size - 8..size].copy_from_slice(&(data.len() as u64).wrapping_mul(8).to_be_bytes());
    for whole in last[..size].chunks_exact(64) {
        block(&mut h, whole);
    }
    h.iter().map(|word| format!("{word:08x}")).collect()
}

// xtask/src/xexdump.rs includes this file as it stands: it may use nothing of the crate.
