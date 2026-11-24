// The texel conversions behind the texture cache's fallback formats (format_support.rs
// says when each is taken): Xenia's texture loads (src/xenia/gpu/shaders/
// texture_load_dxt1_rgba8, _dxt3_rgba8, _dxt5_rgba8, _dxt5a_r8, _dxn_rg8,
// _r16_unorm_float and pixel_formats.xesli; BSD-3), done on the CPU on the untiled
// bytes read_texture gives -- for the DXT formats the BC formats' little-endian blocks
// byte for byte, for the others the host format's texels.

use super::super::texture::{Level, MAX_LEVELS};

/// How a guest texture's bytes become its host format's.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Convert {
    /// Uploaded as read.
    #[default]
    None,
    /// DXT1 to R8G8B8A8.
    Bc1,
    /// DXT2/3 to R8G8B8A8.
    Bc2,
    /// DXT4/5 to R8G8B8A8.
    Bc3,
    /// DXT5A to R8.
    Bc4,
    /// DXN to R8G8.
    Bc5,
    /// 16-bit unsigned normalised channels to half floats.
    Unorm16ToHalf,
    /// R4G4B4A4 to B4G4R4A4: red and blue swapped.
    Rgba4ToBgra4,
}

impl Convert {
    /// The bytes of one 4x4 block in, and of one texel out, for a decompression.
    fn block(self) -> Option<(usize, usize)> {
        match self {
            Convert::Bc1 => Some((8, 4)),
            Convert::Bc2 | Convert::Bc3 => Some((16, 4)),
            Convert::Bc4 => Some((8, 1)),
            Convert::Bc5 => Some((16, 2)),
            _ => None,
        }
    }

    /// Whether it decompresses: its output is one texel a block element.
    pub fn expands(self) -> bool {
        self.block().is_some()
    }

    /// The bytes of one texel out of a decompression.
    pub fn texel_bytes(self) -> u32 {
        self.block().map_or(0, |(_, out)| out as u32)
    }
}

/// `bytes`, laid out as `levels` say, converted; `out_levels` gets where each level lands
/// in the result. A decompressed level is as many texels wide and high as its blocks
/// cover, each slice after the one before.
pub fn convert(how: Convert, levels: &[Level], bytes: &[u8], out_levels: &mut Vec<Level>) -> Vec<u8> {
    out_levels.clear();
    match how.block() {
        None => {
            out_levels.extend_from_slice(levels);
            let mut out = bytes.to_vec();
            match how {
                Convert::Unorm16ToHalf => {
                    for texel in out.chunks_exact_mut(2) {
                        let value = u16::from_le_bytes([texel[0], texel[1]]);
                        texel.copy_from_slice(&half_of(f32::from(value) * (1.0f32 / 65535.0f32)).to_le_bytes());
                    }
                }
                Convert::Rgba4ToBgra4 => {
                    for texel in out.chunks_exact_mut(2) {
                        let v = u16::from_le_bytes([texel[0], texel[1]]);
                        let swapped = (v & 0x0F0F) | ((v >> 8) & 0x00F0) | ((v << 8) & 0xF000);
                        texel.copy_from_slice(&swapped.to_le_bytes());
                    }
                }
                _ => {}
            }
            out
        }
        Some((block_in, texel_out)) => {
            let mut total = 0usize;
            for l in levels.iter().take(MAX_LEVELS) {
                let blocks = l.blocks_wide as usize * l.blocks_high as usize;
                let slices = if blocks == 0 { 0 } else { l.size / (blocks * block_in) };
                let size = blocks * 16 * texel_out * slices;
                // Sixteen-byte offsets suit every texel size.
                let offset = (total + 15) & !15;
                out_levels.push(Level {
                    blocks_wide: l.blocks_wide.wrapping_mul(4),
                    blocks_high: l.blocks_high.wrapping_mul(4),
                    offset,
                    size,
                    ..*l
                });
                total = offset + size;
            }
            let mut out = vec![0u8; total];
            for (l, o) in levels.iter().zip(out_levels.iter()) {
                let (wide, high) = (l.blocks_wide as usize, l.blocks_high as usize);
                let row = wide * 4 * texel_out; // bytes of one row of texels out
                let slices = if wide * high == 0 { 0 } else { o.size / (wide * high * 16 * texel_out) };
                for slice in 0..slices {
                    for by in 0..high {
                        for bx in 0..wide {
                            let at = l.offset + ((slice * high + by) * wide + bx) * block_in;
                            let Some(block) = bytes.get(at..at + block_in) else { continue };
                            let mut texels = [[0u8; 4]; 16];
                            decode_block(how, block, &mut texels);
                            let base = o.offset + slice * high * 4 * row + by * 4 * row + bx * 4 * texel_out;
                            for y in 0..4 {
                                for x in 0..4 {
                                    let to = base + y * row + x * texel_out;
                                    out[to..to + texel_out].copy_from_slice(&texels[y * 4 + x][..texel_out]);
                                }
                            }
                        }
                    }
                }
            }
            out
        }
    }
}

/// One block's sixteen texels, row by row, each its channels first.
pub fn decode_block(how: Convert, block: &[u8], texels: &mut [[u8; 4]; 16]) {
    match how {
        Convert::Bc1 => colour_block(block, true, texels),
        Convert::Bc2 => {
            colour_block(&block[8..16], false, texels);
            let alpha = u64::from_le_bytes(block[0..8].try_into().unwrap_or([0; 8]));
            for (i, texel) in texels.iter_mut().enumerate() {
                let nibble = ((alpha >> (4 * i)) & 15) as u8;
                texel[3] = nibble | (nibble << 4);
            }
        }
        Convert::Bc3 => {
            colour_block(&block[8..16], false, texels);
            let mut alpha = [0u8; 16];
            alpha_block(&block[0..8], &mut alpha);
            for (texel, a) in texels.iter_mut().zip(alpha) {
                texel[3] = a;
            }
        }
        Convert::Bc4 => {
            let mut red = [0u8; 16];
            alpha_block(&block[0..8], &mut red);
            for (texel, r) in texels.iter_mut().zip(red) {
                texel[0] = r;
            }
        }
        Convert::Bc5 => {
            let (mut red, mut green) = ([0u8; 16], [0u8; 16]);
            alpha_block(&block[0..8], &mut red);
            alpha_block(&block[8..16], &mut green);
            for (i, texel) in texels.iter_mut().enumerate() {
                texel[0] = red[i];
                texel[1] = green[i];
            }
        }
        _ => {}
    }
}

/// A 5:6:5 endpoint as 8:8:8, the low bits replicated (XeDXTColorEndpointsToBGR8In10).
fn endpoint(c: u16) -> [u32; 3] {
    let (r, g, b) = (u32::from(c >> 11), u32::from((c >> 5) & 63), u32::from(c & 31));
    [(r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2)]
}

/// A DXT colour block: four colours, or with `punchthrough` and colour 0 no greater than
/// colour 1, three and a transparent black (XeDXTOpaqueRowToRGB8, XeDXT1TransRowToRGBA8:
/// thirds and halves truncated).
fn colour_block(block: &[u8], punchthrough: bool, texels: &mut [[u8; 4]; 16]) {
    let c0 = u16::from_le_bytes([block[0], block[1]]);
    let c1 = u16::from_le_bytes([block[2], block[3]]);
    let codes = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
    let (e0, e1) = (endpoint(c0), endpoint(c1));
    let three = punchthrough && c0 <= c1;
    for (i, texel) in texels.iter_mut().enumerate() {
        let code = (codes >> (2 * i)) & 3;
        let mut rgba = [0u32, 0, 0, 255];
        for k in 0..3 {
            rgba[k] = match (three, code) {
                (_, 0) => e0[k],
                (_, 1) => e1[k],
                (false, 2) => (2 * e0[k] + e1[k]) / 3,
                (false, _) => (e0[k] + 2 * e1[k]) / 3,
                (true, 2) => (e0[k] + e1[k]) >> 1,
                (true, _) => 0,
            };
        }
        if three && code == 3 {
            rgba[3] = 0;
        }
        *texel = [rgba[0] as u8, rgba[1] as u8, rgba[2] as u8, rgba[3] as u8];
    }
}

/// A DXT5 alpha block (DXT5A's and DXN's channels too): eight steps when the first
/// endpoint is greater, else six and the constants 0 and 255 (XeDXT58StepRowToA8,
/// XeDXT56StepRowToA8: sevenths and fifths truncated).
fn alpha_block(block: &[u8], out: &mut [u8; 16]) {
    let (a0, a1) = (u32::from(block[0]), u32::from(block[1]));
    let mut codes = 0u64;
    for (k, &byte) in block[2..8].iter().enumerate() {
        codes |= u64::from(byte) << (8 * k);
    }
    for (i, value) in out.iter_mut().enumerate() {
        let code = ((codes >> (3 * i)) & 7) as u32;
        *value = match code {
            0 => a0,
            1 => a1,
            _ if a0 > a1 => (a0 * (8 - code) + a1 * (code - 1)) / 7,
            6 => 0,
            7 => 255,
            _ => (a0 * (6 - code) + a1 * (code - 1)) / 5,
        } as u8;
    }
}

/// A float as a half, rounded to nearest, ties to even.
pub fn half_of(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xFF) as i32;
    let mantissa = bits & 0x007F_FFFF;
    if exponent == 0xFF {
        // Infinity, or a NaN kept quiet.
        return sign | 0x7C00 | if mantissa != 0 { 0x0200 } else { 0 };
    }
    let unbiased = exponent - 127;
    if unbiased > 15 {
        return sign | 0x7C00;
    }
    if unbiased >= -14 {
        // Normal: 10 of the 23 mantissa bits, the rest rounded.
        let half = (((unbiased + 15) as u32) << 10) | (mantissa >> 13);
        let rest = mantissa & 0x1FFF;
        let rounded = if rest > 0x1000 || (rest == 0x1000 && half & 1 != 0) { half + 1 } else { half };
        return sign | rounded as u16;
    }
    if unbiased < -25 {
        return sign;
    }
    // Subnormal: the implicit one made explicit, shifted into place and rounded.
    let full = mantissa | 0x0080_0000;
    let shift = (-14 - unbiased) as u32 + 13;
    let half = full >> shift;
    let rest = full & ((1 << shift) - 1);
    let middle = 1u32 << (shift - 1);
    let rounded = if rest > middle || (rest == middle && half & 1 != 0) { half + 1 } else { half };
    sign | rounded as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halves() {
        assert_eq!(half_of(0.0), 0);
        assert_eq!(half_of(1.0), 0x3C00);
        assert_eq!(half_of(0.5), 0x3800);
        assert_eq!(half_of(65504.0), 0x7BFF);
        assert_eq!(half_of(1e6), 0x7C00);
        assert_eq!(half_of(6.1035156e-5), 0x0400); // smallest normal
        assert_eq!(half_of(5.9604645e-8), 0x0001); // smallest subnormal
    }
}
