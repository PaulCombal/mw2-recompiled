// FFmpeg's inverse MDCT (libavutil/tx_template.c, mdct_inv over the split-radix fftN_ns
// codelets), as its C code does it: the same tables from the same libm calls, the same
// permutation, the same operations in the same order, so the same floats come out. This
// build of FFmpeg has no assembly, so its C is what the decoder ran.
//
// Copyright (c) Lynne
// Copyright (c) 2008 Loren Merritt
// Copyright (c) 2002 Fabrice Bellard
// Part of a port of FFmpeg, LGPL 2.1 or later: see NOTICE.md in this folder.

use std::f64::consts::{FRAC_PI_2, PI};
use std::sync::LazyLock;

#[derive(Clone, Copy, Default)]
pub struct Complex {
    pub re: f32,
    pub im: f32,
}

// ff_tx_tab_N for N = 8 ... 256: cos(2 pi i / N) for i < N/4, then 0.
struct CosTables {
    tables: Vec<Vec<f32>>, // indexed by log2(N)
}

static COS: LazyLock<CosTables> = LazyLock::new(|| CosTables {
    tables: (0..=8)
        .map(|log2| {
            if log2 < 3 {
                return Vec::new();
            }
            let len = 1usize << log2;
            let freq = 2.0 * PI / len as f64;
            let mut table: Vec<f32> = (0..len / 4).map(|i| (i as f64 * freq).cos() as f32).collect();
            table.push(0.0);
            table
        })
        .collect(),
});

fn cos_table(len: usize) -> &'static [f32] {
    &COS.tables[len.trailing_zeros() as usize]
}

// BF(x, y, a, b): x = a - b, y = a + b.
macro_rules! bf {
    ($x:expr, $y:expr, $a:expr, $b:expr) => {{
        let (a, b) = ($a, $b);
        $x = a - b;
        $y = a + b;
    }};
}

// CMUL(dre, dim, are, aim, bre, bim).
macro_rules! cmul {
    ($dre:expr, $dim:expr, $are:expr, $aim:expr, $bre:expr, $bim:expr) => {{
        let (are, aim, bre, bim) = ($are, $aim, $bre, $bim);
        $dre = are * bre - aim * bim;
        $dim = are * bim + aim * bre;
    }};
}

// BUTTERFLIES(a0, a1, a2, a3), with the temporaries t1, t2, t5, t6 given.
#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn butterflies(
    a0: &mut Complex,
    a1: &mut Complex,
    a2: &mut Complex,
    a3: &mut Complex,
    t1: f32,
    t2: f32,
    t5: f32,
    t6: f32,
) {
    let (r0, i0, r1, i1) = (a0.re, a0.im, a1.re, a1.im);
    let (t3, t5) = (t5 - t1, t5 + t1);
    bf!(a2.re, a0.re, r0, t5);
    bf!(a3.im, a1.im, i1, t3);
    let (t4, t6) = (t2 - t6, t2 + t6);
    bf!(a3.re, a1.re, r1, t4);
    bf!(a2.im, a0.im, i0, t6);
}

// TRANSFORM(a0, a1, a2, a3, wre, wim).
#[inline(always)]
fn transform(a0: &mut Complex, a1: &mut Complex, a2: &mut Complex, a3: &mut Complex, wre: f32, wim: f32) {
    let (t1, t2, t5, t6);
    cmul!(t1, t2, a2.re, a2.im, wre, -wim);
    cmul!(t5, t6, a3.re, a3.im, wre, wim);
    butterflies(a0, a1, a2, a3, t1, t2, t5, t6);
}

// Four elements of z, which must differ.
#[inline(always)]
fn four(z: &mut [Complex], a: usize, b: usize, c: usize, d: usize) -> [&mut Complex; 4] {
    z.get_disjoint_mut([a, b, c, d]).expect("four different elements")
}

// ff_tx_fft_sr_combine: z[0 .. 8 len], the table from its first entry. Each step takes eight
// elements of each quarter, eight cosines going up and eight going down from cos[2 len].
fn sr_combine(z: &mut [Complex], cos: &[f32], len: usize) {
    let o1 = 2 * len;
    let (q0, rest) = z.split_at_mut(o1);
    let (q1, rest) = rest.split_at_mut(o1);
    let (q2, q3) = rest.split_at_mut(o1);
    let steps = q0
        .chunks_exact_mut(8)
        .zip(q1.chunks_exact_mut(8))
        .zip(q2.chunks_exact_mut(8))
        .zip(q3.chunks_exact_mut(8))
        .zip(cos[..o1].chunks_exact(8))
        .zip(cos[1..=o1].rchunks_exact(8));
    for (((((z0, z1), z2), z3), c), m) in steps {
        let mut step = |k: usize, w: usize| transform(&mut z0[k], &mut z1[k], &mut z2[k], &mut z3[k], c[k], m[w]);
        step(0, 7);
        step(2, 5);
        step(4, 3);
        step(6, 1);
        step(1, 6);
        step(3, 4);
        step(5, 2);
        step(7, 0);
    }
}

fn fft2(z: &mut [Complex]) {
    let tmp_re = z[0].re - z[1].re;
    z[0].re += z[1].re;
    let tmp_im = z[0].im - z[1].im;
    z[0].im += z[1].im;
    z[1] = Complex { re: tmp_re, im: tmp_im };
}

fn fft4(z: &mut [Complex]) {
    let (t3, t1, t8, t6, t4, t2, t7, t5);
    bf!(t3, t1, z[0].re, z[1].re);
    bf!(t8, t6, z[3].re, z[2].re);
    bf!(z[2].re, z[0].re, t1, t6);
    bf!(t4, t2, z[0].im, z[1].im);
    bf!(t7, t5, z[2].im, z[3].im);
    bf!(z[3].im, z[1].im, t4, t8);
    bf!(z[3].re, z[1].re, t3, t7);
    bf!(z[2].im, z[0].im, t2, t5);
}

fn fft8(z: &mut [Complex]) {
    let cos = cos_table(8)[1];
    fft4(z);
    let (t1, t2, t5, t6);
    bf!(t1, z[5].re, z[4].re, -z[5].re);
    bf!(t2, z[5].im, z[4].im, -z[5].im);
    bf!(t5, z[7].re, z[6].re, -z[7].re);
    bf!(t6, z[7].im, z[6].im, -z[7].im);
    let [a0, a1, a2, a3] = four(z, 0, 2, 4, 6);
    butterflies(a0, a1, a2, a3, t1, t2, t5, t6);
    let [a0, a1, a2, a3] = four(z, 1, 3, 5, 7);
    transform(a0, a1, a2, a3, cos, cos);
}

fn fft16(z: &mut [Complex]) {
    let cos = cos_table(16);
    let (cos_16_1, cos_16_2, cos_16_3) = (cos[1], cos[2], cos[3]);
    fft8(&mut z[0..8]);
    fft4(&mut z[8..12]);
    fft4(&mut z[12..16]);
    let (t1, t2, t5, t6) = (z[8].re, z[8].im, z[12].re, z[12].im);
    let [a0, a1, a2, a3] = four(z, 0, 4, 8, 12);
    butterflies(a0, a1, a2, a3, t1, t2, t5, t6);
    let [a0, a1, a2, a3] = four(z, 2, 6, 10, 14);
    transform(a0, a1, a2, a3, cos_16_2, cos_16_2);
    let [a0, a1, a2, a3] = four(z, 1, 5, 9, 13);
    transform(a0, a1, a2, a3, cos_16_1, cos_16_3);
    let [a0, a1, a2, a3] = four(z, 3, 7, 11, 15);
    transform(a0, a1, a2, a3, cos_16_3, cos_16_1);
}

// fftN_ns, in place, on input already in the split-radix order.
fn fft(z: &mut [Complex]) {
    let n = z.len();
    match n {
        2 => fft2(z),
        4 => fft4(z),
        8 => fft8(z),
        16 => fft16(z),
        _ => {
            let n4 = n / 4;
            fft(&mut z[..n / 2]);
            fft(&mut z[n4 * 2..n4 * 3]);
            fft(&mut z[n4 * 3..]);
            sr_combine(z, cos_table(n), n4 >> 1);
        }
    }
}

// split_radix_permutation(i, len, inv) of libavutil/tx.c.
fn split_radix_permutation(i: i32, len: i32, inv: bool) -> i32 {
    let len = len >> 1;
    if len <= 1 {
        return i & 1;
    }
    if i & len == 0 {
        return split_radix_permutation(i, len, inv) * 2;
    }
    let len = len >> 1;
    split_radix_permutation(i, len, inv) * 4 + 1 - 2 * (i32::from(i & len == 0) ^ i32::from(inv))
}

/// An inverse MDCT of `len` coefficients to `len` samples (FFmpeg's half IMDCT).
pub struct Mdct {
    len: usize,
    map: Vec<usize>,   // pre-shifted by one, as FFmpeg keeps it
    exp: Vec<Complex>, // len/2 in the FFT's order, then len/2 in order
    z: Vec<Complex>,
}

impl Mdct {
    pub fn new(len: usize, scale: f32) -> Self {
        let half = len / 2;
        // The FFT's gather map (ff_tx_gen_ptwo_revtab), which the MDCT takes as its own.
        let map: Vec<i32> =
            (0..half as i32).map(|i| -split_radix_permutation(i, half as i32, true) & (half as i32 - 1)).collect();
        // ff_tx_mdct_gen_exp, with the float scale read as a double.
        let scale_d = f64::from(scale);
        let theta = (if scale_d < 0.0 { half as f64 } else { 0.0 }) + 1.0 / 8.0;
        let root = scale_d.abs().sqrt();
        let mut exp = vec![Complex::default(); 2 * half];
        for i in 0..half {
            let alpha = FRAC_PI_2 * (i as f64 + theta) / half as f64;
            exp[half + i] = Complex { re: (alpha.cos() * root) as f32, im: (alpha.sin() * root) as f32 };
        }
        for i in 0..half {
            exp[i] = exp[half + map[i] as usize];
        }
        LazyLock::force(&COS);
        Self { len, map: map.iter().map(|&k| (k as usize) << 1).collect(), exp, z: vec![Complex::default(); half] }
    }

    /// ff_tx_mdct_inv: `src` holds `len` coefficients, `dst` gets `len` samples.
    pub fn inverse(&mut self, dst: &mut [f32], src: &[f32]) {
        let len = self.len;
        let (len2, len4) = (len / 2, len / 4);
        let z = &mut self.z;
        for ((zi, &k), e) in z.iter_mut().zip(&self.map).zip(&self.exp[..len2]) {
            let (re, im) = (src[len - 1 - k], src[k]);
            cmul!(zi.re, zi.im, re, im, e.re, e.im);
        }
        fft(z);
        // The post-rotation, from the middle outwards, written as floats: z[i] is dst[2i]
        // and dst[2i + 1].
        let exp = &self.exp[len2..];
        let dst = &mut dst[..len];
        let (low, high) = dst.split_at_mut(len / 2);
        let (z_low, z_high) = z.split_at(len4);
        let (exp_low, exp_high) = exp[..len2].split_at(len4);
        let pairs = low
            .rchunks_exact_mut(2)
            .zip(high.chunks_exact_mut(2))
            .zip(z_low.iter().rev().zip(z_high))
            .zip(exp_low.iter().rev().zip(exp_high));
        for (((d1, d0), (z1, z0)), (e1, e0)) in pairs {
            let src1 = Complex { re: z1.im, im: z1.re };
            let src0 = Complex { re: z0.im, im: z0.re };
            cmul!(d1[0], d0[1], src1.re, src1.im, e1.im, e1.re);
            cmul!(d0[0], d1[1], src0.re, src0.im, e0.im, e0.re);
        }
    }
}
