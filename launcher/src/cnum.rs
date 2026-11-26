// Numbers read out of text as the C library reads them: atoi, strtol, strtoul and
// strtoull, and scanf's %u, which the launcher's files were read with before it was
// Rust, so that a file a player edited by hand reads the same. `long` is 64 bits on
// Linux and 32 on Windows, which changes where a number too big saturates.
#![deny(unsafe_code)]

#[cfg(windows)]
const LONG_BITS: u32 = 32;
#[cfg(not(windows))]
const LONG_BITS: u32 = 64;

fn space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\x0B' | b'\x0C' | b'\r')
}

fn digit(byte: u8, base: u32) -> Option<u32> {
    let value = match byte {
        b'0'..=b'9' => u32::from(byte - b'0'),
        b'a'..=b'z' => u32::from(byte - b'a') + 10,
        b'A'..=b'Z' => u32::from(byte - b'A') + 10,
        _ => return None,
    };
    (value < base).then_some(value)
}

// What strto* reads: past spaces, a sign, base 16's 0x when a digit follows it, then the
// digits. The magnitude, saturated at u64::MAX; whether it was negative; whether it
// overflowed `bits`; and where it ended, or None when there was no digit (the C's end
// pointer is then the text's start).
struct Read {
    magnitude: u64,
    negative: bool,
    overflow: bool,
    end: Option<usize>,
}

fn read(text: &[u8], base: u32, bits: u32) -> Read {
    let mut at = 0;
    while text.get(at).is_some_and(|&byte| space(byte)) {
        at += 1;
    }
    let negative = text.get(at) == Some(&b'-');
    if matches!(text.get(at), Some(b'-' | b'+')) {
        at += 1;
    }
    if base == 16
        && text.get(at) == Some(&b'0')
        && matches!(text.get(at + 1), Some(b'x' | b'X'))
        && text.get(at + 2).is_some_and(|&byte| digit(byte, 16).is_some())
    {
        at += 2;
    }
    let limit = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
    let (mut magnitude, mut overflow, start) = (0u64, false, at);
    while let Some(value) = text.get(at).and_then(|&byte| digit(byte, base)) {
        match magnitude.checked_mul(u64::from(base)).and_then(|m| m.checked_add(u64::from(value))) {
            Some(next) if next <= limit => magnitude = next,
            _ => {
                overflow = true;
                magnitude = limit;
            }
        }
        at += 1;
    }
    Read { magnitude, negative, overflow, end: (at > start).then_some(at) }
}

/// strtoul and strtoull of `bits` (32 or 64): ULONG_MAX on overflow, a negative number
/// wrapped, and where it ended, None for no number.
pub fn strtoul(text: &[u8], base: u32, bits: u32) -> (u64, Option<usize>) {
    let read = read(text, base, bits);
    let mask = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
    let value = if read.overflow {
        mask
    } else if read.negative {
        read.magnitude.wrapping_neg() & mask
    } else {
        read.magnitude
    };
    (value, read.end)
}

/// strtol: LONG_MIN or LONG_MAX on overflow.
pub fn strtol(text: &[u8], base: u32) -> (i64, Option<usize>) {
    let read = read(text, base, 64);
    let (most, least) = if LONG_BITS == 64 { (i64::MAX, i64::MIN) } else { (i64::from(i32::MAX), i64::from(i32::MIN)) };
    let value = if read.negative {
        if read.overflow || read.magnitude > least.unsigned_abs() {
            least
        } else {
            0i64.wrapping_sub_unsigned(read.magnitude)
        }
    } else if read.overflow || read.magnitude > most as u64 {
        most
    } else {
        read.magnitude as i64
    };
    (value, read.end)
}

/// atoi: strtol's long, as an int.
pub fn atoi(text: &[u8]) -> i32 {
    strtol(text, 10).0 as i32
}

/// scanf's %u: past spaces, strtoul's number of a long, as an unsigned int; and the rest
/// of the text, or None when there is no number.
pub fn scan_u(text: &[u8]) -> Option<(u32, &[u8])> {
    let (value, end) = strtoul(text, 10, LONG_BITS);
    end.map(|end| (value as u32, &text[end..]))
}

/// The text's first line up to a zero byte, as a C string of it would end there.
pub fn c_str(text: &[u8]) -> &[u8] {
    text.split(|&byte| byte == 0).next().unwrap_or_default()
}
