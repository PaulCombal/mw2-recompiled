// Numbers in either byte order: the PC's zones are little-endian, the 360's big-endian.

/// `size` bytes (at most 8) at the start of `p` as a number.
pub fn load(p: &[u8], size: u32, big: bool) -> u64 {
    let p = &p[..size as usize];
    let mut v: u64 = 0;
    if big {
        for &b in p {
            v = (v << 8) | u64::from(b);
        }
    } else {
        for &b in p.iter().rev() {
            v = (v << 8) | u64::from(b);
        }
    }
    v
}

/// `v`'s low `size` bytes into the start of `p`.
pub fn store(p: &mut [u8], size: u32, mut v: u64, big: bool) {
    let p = &mut p[..size as usize];
    if big {
        for b in p.iter_mut().rev() {
            *b = v as u8;
            v >>= 8;
        }
    } else {
        for b in p.iter_mut() {
            *b = v as u8;
            v >>= 8;
        }
    }
}

pub fn load_f32(p: &[u8], big: bool) -> f32 {
    f32::from_bits(load(p, 4, big) as u32)
}

pub fn store_f32(p: &mut [u8], v: f32, big: bool) {
    store(p, 4, u64::from(v.to_bits()), big);
}

/// A float as x86-64's conversion truncates it to a 64-bit integer (cvttsd2si): the
/// "integer indefinite" value, i64::MIN, for NaN and anything out of range, where Rust's
/// `as` saturates.
pub fn truncate(f: f64) -> i64 {
    const LIMIT: f64 = (1u64 << 63) as f64;
    if (-LIMIT..LIMIT).contains(&f) { f as i64 } else { i64::MIN }
}

/// A float as x86-64's conversion truncates it to a 32-bit integer (cvttss2si,
/// cvttsd2si): i32::MIN for NaN and anything out of range.
pub fn truncate32(f: f64) -> i32 {
    if f > -2147483649.0 && f < 2147483648.0 { f as i32 } else { i32::MIN }
}
