//! HyperFrames' frame extraction as a JPEG file: ffmpeg's `SDR_JPEG_AS_BT601_FULL_RANGE_FILTER`
//! (nearest-chroma expansion to RGB, BT.601 full range back to 4:2:0) and its `mjpeg` encoder at
//! `-q:v 2`, from a decoded NV12 frame. The page decodes the file as HyperFrames' page decodes
//! ffmpeg's.

use std::thread;

const QUANT: [i32; 64] = [
    8, 4, 4, 5, 6, 6, 7, 8, 4, 4, 5, 6, 6, 7, 8, 9, 4, 5, 6, 6, 7, 8, 8, 9, 5, 5, 6, 6, 7, 8, 9,
    10, 5, 6, 6, 7, 8, 8, 10, 12, 6, 6, 7, 8, 8, 10, 12, 14, 6, 6, 7, 8, 9, 11, 14, 17, 6, 7, 8, 9,
    11, 14, 17, 20,
];

/// `n / (8 * QUANT[i])` is `(n * RECIPROCAL[i]) >> 22` for every `n` below 2^15. A coefficient's
/// magnitude is at most 8 x 2048.
const RECIPROCAL: [u32; 64] = {
    let mut table = [0; 64];
    let mut i = 0;
    while i < 64 {
        table[i] = (1 << 22) / (8 * QUANT[i] as u32) + 1;
        i += 1;
    }
    table
};

const NATURAL_TO_ZIGZAG: [usize; 64] = {
    let mut table = [0; 64];
    let mut i = 0;
    while i < 64 {
        table[ZIGZAG[i]] = i;
        i += 1;
    }
    table
};

const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

struct Table {
    bits: [u8; 16],
    values: &'static [u8],
}

const DC_LUMA: Table = Table {
    bits: [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0],
    values: &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
};
const DC_CHROMA: Table = Table {
    bits: [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0],
    values: &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
};
const AC_LUMA: Table = Table {
    bits: [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 125],
    values: &[
        1, 2, 3, 0, 4, 17, 5, 18, 33, 49, 65, 6, 19, 81, 97, 7, 34, 113, 20, 50, 129, 145, 161, 8,
        35, 66, 177, 193, 21, 82, 209, 240, 36, 51, 98, 114, 130, 9, 10, 22, 23, 24, 25, 26, 37,
        38, 39, 40, 41, 42, 52, 53, 54, 55, 56, 57, 58, 67, 68, 69, 70, 71, 72, 73, 74, 83, 84, 85,
        86, 87, 88, 89, 90, 99, 100, 101, 102, 103, 104, 105, 106, 115, 116, 117, 118, 119, 120,
        121, 122, 131, 132, 133, 134, 135, 136, 137, 138, 146, 147, 148, 149, 150, 151, 152, 153,
        154, 162, 163, 164, 165, 166, 167, 168, 169, 170, 178, 179, 180, 181, 182, 183, 184, 185,
        186, 194, 195, 196, 197, 198, 199, 200, 201, 202, 210, 211, 212, 213, 214, 215, 216, 217,
        218, 225, 226, 227, 228, 229, 230, 231, 232, 233, 234, 241, 242, 243, 244, 245, 246, 247,
        248, 249, 250,
    ],
};
const AC_CHROMA: Table = Table {
    bits: [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 119],
    values: &[
        0, 1, 2, 3, 17, 4, 5, 33, 49, 6, 18, 65, 81, 7, 97, 113, 19, 34, 50, 129, 8, 20, 66, 145,
        161, 177, 193, 9, 35, 51, 82, 240, 21, 98, 114, 209, 10, 22, 36, 52, 225, 37, 241, 23, 24,
        25, 26, 38, 39, 40, 41, 42, 53, 54, 55, 56, 57, 58, 67, 68, 69, 70, 71, 72, 73, 74, 83, 84,
        85, 86, 87, 88, 89, 90, 99, 100, 101, 102, 103, 104, 105, 106, 115, 116, 117, 118, 119,
        120, 121, 122, 130, 131, 132, 133, 134, 135, 136, 137, 138, 146, 147, 148, 149, 150, 151,
        152, 153, 154, 162, 163, 164, 165, 166, 167, 168, 169, 170, 178, 179, 180, 181, 182, 183,
        184, 185, 186, 194, 195, 196, 197, 198, 199, 200, 201, 202, 210, 211, 212, 213, 214, 215,
        216, 217, 218, 226, 227, 228, 229, 230, 231, 232, 233, 234, 242, 243, 244, 245, 246, 247,
        248, 249, 250,
    ],
};

const THREADS: usize = 4;

struct Codes([(u16, u8); 256]);

impl Codes {
    fn of(table: &Table) -> Codes {
        let mut codes = [(0, 0); 256];
        let mut code = 0u16;
        let mut values = table.values.iter();
        for (length, &count) in (1..=16u8).zip(&table.bits) {
            for _ in 0..count {
                codes[*values.next().unwrap() as usize] = (code, length);
                code += 1;
            }
            code <<= 1;
        }
        Codes(codes)
    }
}

#[derive(Clone, Copy)]
struct Conversion {
    luma: i32,
    offset: i32,
    red: i32,
    green_v: i32,
    green_u: i32,
    blue: i32,
}

impl Conversion {
    fn new(bt709: bool, full: bool) -> Conversion {
        let mut coefficients: [i64; 4] = if bt709 {
            [117489, 138438, -13975, -34925]
        } else {
            [104597, 132201, -25675, -53279]
        };
        let (mut luma, mut offset) = (65536i64, 0i64);
        if full {
            coefficients = coefficients.map(|c| c * 224 / 255);
        } else {
            luma = 65536 * 255 / 219;
            offset = 16 << 16;
        }
        let rounded = |value: i64| ((value + 32768) >> 16) as i32;
        let [red, blue, green_u, green_v] = coefficients;
        Conversion {
            luma: rounded(luma * 8192),
            offset: rounded(offset * 512),
            red: rounded(red * 8192),
            green_v: rounded(green_v * 8192),
            green_u: rounded(green_u * 8192),
            blue: rounded(blue * 8192),
        }
    }
}

struct Source<'a> {
    nv12: &'a [u8],
    width: usize,
    height: usize,
    chroma_row: usize,
    k: Conversion,
}

impl Source<'_> {
    /// swscale's YUV to RGB for one pixel. Sums that leave `i32` saturate, and those clamp to 0
    /// or 255 either way.
    #[inline(always)]
    fn rgb(&self, y: u8, u: u8, v: u8) -> [i32; 3] {
        let k = &self.k;
        let base = ((i32::from(y) << 9) - k.offset) * k.luma + 2_097_152;
        let (u, v) = ((i32::from(u) - 128) << 9, (i32::from(v) - 128) << 9);
        let level = |value: i32| (value >> 22).clamp(0, 255);
        [
            level(base.saturating_add(v * k.red)),
            level(base + v * k.green_v + u * k.green_u),
            level(base.saturating_add(u * k.blue)),
        ]
    }

    fn lines(&self, y: usize) -> (&[u8], &[u8]) {
        let at = self.width * self.height + (y >> 1) * self.chroma_row;
        (
            &self.nv12[y * self.width..(y + 1) * self.width],
            &self.nv12[at..at + self.chroma_row],
        )
    }

    fn luma_row(&self, y: usize, out: &mut [u8]) {
        let (luma, chroma) = self.lines(y);
        let pairs = out
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip(luma.as_chunks::<2>().0)
            .zip(chroma.as_chunks::<2>().0);
        for ((out, luma), uv) in pairs {
            out[0] = to_luma(self.rgb(luma[0], uv[0], uv[1]));
            out[1] = to_luma(self.rgb(luma[1], uv[0], uv[1]));
        }
        if self.width % 2 == 1 {
            let x = self.width - 1;
            out[x] = to_luma(self.rgb(luma[x], chroma[x], chroma[x + 1]));
        }
    }

    /// Luma of row `y`, and the chroma ffmpeg takes from it: each pair's RGB sum.
    fn chroma_row(&self, y: usize, out: &mut [u8], cb: &mut [u8], cr: &mut [u8]) {
        let (luma, chroma) = self.lines(y);
        let pairs = out
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip(luma.as_chunks::<2>().0)
            .zip(chroma.as_chunks::<2>().0);
        for (((out, luma), uv), (cb, cr)) in pairs.zip(cb.iter_mut().zip(cr.iter_mut())) {
            let a = self.rgb(luma[0], uv[0], uv[1]);
            let b = self.rgb(luma[1], uv[0], uv[1]);
            out[0] = to_luma(a);
            out[1] = to_luma(b);
            let sum = [a[0] + b[0], a[1] + b[1], a[2] + b[2]];
            *cb = ((-5537 * sum[0] - 10848 * sum[1] + 16385 * sum[2] + 8_388_608) >> 16)
                .clamp(0, 255) as u8;
            *cr = ((16386 * sum[0] - 13730 * sum[1] - 2656 * sum[2] + 8_388_608) >> 16)
                .clamp(0, 255) as u8;
        }
        if self.width % 2 == 1 {
            let x = self.width - 1;
            out[x] = to_luma(self.rgb(luma[x], chroma[x], chroma[x + 1]));
        }
    }
}

#[inline(always)]
fn to_luma([r, g, b]: [i32; 3]) -> u8 {
    ((9798 * r + 19235 * g + 3735 * b + 16383) >> 15).clamp(0, 255) as u8
}

struct Plane {
    samples: Vec<u8>,
    width: usize,
    height: usize,
}

impl Plane {
    /// The quantised levels of block (`bx`, `by`) in zigzag order. Samples past the edge repeat
    /// the last row and column.
    fn block(&self, bx: usize, by: usize) -> [i32; 64] {
        let (x0, y0) = (bx * 8, by * 8);
        #[cfg(target_arch = "aarch64")]
        if x0 + 8 <= self.width && y0 + 8 <= self.height {
            return neon::block(&self.samples[y0 * self.width + x0..], self.width);
        }
        let mut samples = [[0i32; 8]; 8];
        if x0 + 8 <= self.width && y0 + 8 <= self.height {
            for (j, column) in samples.iter_mut().enumerate() {
                for (r, sample) in column.iter_mut().enumerate() {
                    *sample = i32::from(self.samples[(y0 + r) * self.width + x0 + j]) - 128;
                }
            }
        } else {
            for (j, column) in samples.iter_mut().enumerate() {
                for (r, sample) in column.iter_mut().enumerate() {
                    let y = (y0 + r).min(self.height - 1);
                    let x = (x0 + j).min(self.width - 1);
                    *sample = i32::from(self.samples[y * self.width + x]) - 128;
                }
            }
        }
        let rows = fdct::<false>(&samples);
        let mut transposed = [[0i32; 8]; 8];
        for (u, row) in rows.iter().enumerate() {
            for (r, &value) in row.iter().enumerate() {
                transposed[r][u] = value;
            }
        }
        let columns = fdct::<true>(&transposed);
        let mut levels = [0i32; 64];
        for (v, row) in columns.iter().enumerate() {
            let quantised = quantise(row, v);
            for (u, &value) in quantised.iter().enumerate() {
                levels[NATURAL_TO_ZIGZAG[v * 8 + u]] = value;
            }
        }
        levels[0] = (columns[0][0] + 32) >> 6;
        levels
    }
}

type Block = [[i32; 8]; 8];

/// [`Plane::block`] for a whole block on NEON: the same `jfdctint` butterflies and quantiser,
/// eight lanes at a time, sixteen-bit sums and thirty-two-bit products as in libjpeg-turbo's
/// NEON transform, so the levels are equal to the scalar ones.
#[cfg(target_arch = "aarch64")]
mod neon {
    use std::arch::aarch64::*;

    use super::{NATURAL_TO_ZIGZAG, QUANT, RECIPROCAL};

    struct Row {
        reciprocal: [u32; 8],
        step: [i32; 8],
        tie: [u32; 8],
    }

    const ROWS: [Row; 8] = {
        let mut rows = [const {
            Row {
                reciprocal: [0; 8],
                step: [0; 8],
                tie: [0; 8],
            }
        }; 8];
        let mut i = 0;
        while i < 64 {
            let (v, u, q) = (i / 8, i % 8, QUANT[i]);
            rows[v].reciprocal[u] = RECIPROCAL[i];
            rows[v].step[u] = 8 * q;
            rows[v].tie[u] = if q == 4 || q == 8 { u32::MAX } else { 0 };
            i += 1;
        }
        rows
    };

    #[inline(always)]
    unsafe fn transpose(m: [int16x8_t; 8]) -> [int16x8_t; 8] {
        unsafe {
            let a: [int16x8_t; 8] = std::array::from_fn(|k| {
                let (x, y) = (m[k & !1], m[k | 1]);
                if k & 1 == 0 { vtrn1q_s16(x, y) } else { vtrn2q_s16(x, y) }
            });
            let b: [int32x4_t; 8] = std::array::from_fn(|k| {
                let (i, j) = ((k & !3) | (k & 1), (k & !3) | (k & 1) | 2);
                let (x, y) = (vreinterpretq_s32_s16(a[i]), vreinterpretq_s32_s16(a[j]));
                if k & 2 == 0 { vtrn1q_s32(x, y) } else { vtrn2q_s32(x, y) }
            });
            std::array::from_fn(|k| {
                let (i, j) = (k & 3, (k & 3) | 4);
                let (x, y) = (vreinterpretq_s64_s32(b[i]), vreinterpretq_s64_s32(b[j]));
                vreinterpretq_s16_s64(if k & 4 == 0 { vtrn1q_s64(x, y) } else { vtrn2q_s64(x, y) })
            })
        }
    }

    #[inline(always)]
    unsafe fn product(x: int16x8_t, k: i16) -> (int32x4_t, int32x4_t) {
        unsafe { (vmull_n_s16(vget_low_s16(x), k), vmull_high_n_s16(x, k)) }
    }

    #[inline(always)]
    unsafe fn add(a: (int32x4_t, int32x4_t), b: (int32x4_t, int32x4_t)) -> (int32x4_t, int32x4_t) {
        unsafe { (vaddq_s32(a.0, b.0), vaddq_s32(a.1, b.1)) }
    }

    #[inline(always)]
    unsafe fn descale<const N: i32>(x: (int32x4_t, int32x4_t)) -> int16x8_t {
        unsafe { vcombine_s16(vrshrn_n_s32::<N>(x.0), vrshrn_n_s32::<N>(x.1)) }
    }

    #[inline(always)]
    unsafe fn fdct<const SECOND: bool, const N: i32>(d: [int16x8_t; 8]) -> [int16x8_t; 8] {
        unsafe {
            let (t0, t7) = (vaddq_s16(d[0], d[7]), vsubq_s16(d[0], d[7]));
            let (t1, t6) = (vaddq_s16(d[1], d[6]), vsubq_s16(d[1], d[6]));
            let (t2, t5) = (vaddq_s16(d[2], d[5]), vsubq_s16(d[2], d[5]));
            let (t3, t4) = (vaddq_s16(d[3], d[4]), vsubq_s16(d[3], d[4]));
            let (t10, t13) = (vaddq_s16(t0, t3), vsubq_s16(t0, t3));
            let (t11, t12) = (vaddq_s16(t1, t2), vsubq_s16(t1, t2));
            let (o0, o4) = if SECOND {
                (
                    vrshrq_n_s16::<1>(vaddq_s16(t10, t11)),
                    vrshrq_n_s16::<1>(vsubq_s16(t10, t11)),
                )
            } else {
                (
                    vshlq_n_s16::<1>(vaddq_s16(t10, t11)),
                    vshlq_n_s16::<1>(vsubq_s16(t10, t11)),
                )
            };
            let z1 = product(vaddq_s16(t12, t13), 4433);
            let o2 = descale::<N>(add(z1, product(t13, 6270)));
            let o6 = descale::<N>(add(z1, product(t12, -15137)));
            let (t10, t11) = (vaddq_s16(t4, t7), vaddq_s16(t5, t6));
            let (t12, t13) = (vaddq_s16(t4, t6), vaddq_s16(t5, t7));
            let z5 = product(vaddq_s16(t12, t13), 9633);
            let (z1, z2) = (product(t10, -7373), product(t11, -20995));
            let z3 = add(product(t12, -16069), z5);
            let z4 = add(product(t13, -3196), z5);
            let o7 = descale::<N>(add(add(product(t4, 2446), z1), z3));
            let o5 = descale::<N>(add(add(product(t5, 16819), z2), z4));
            let o3 = descale::<N>(add(add(product(t6, 25172), z2), z3));
            let o1 = descale::<N>(add(add(product(t7, 12299), z1), z4));
            [o0, o1, o2, o3, o4, o5, o6, o7]
        }
    }

    #[inline(always)]
    unsafe fn quantise(c: int32x4_t, row: &Row, half: usize) -> int32x4_t {
        unsafe {
            let n = vabsq_s32(c);
            let k = vreinterpretq_s32_u32(vshrq_n_u32::<22>(vmulq_u32(
                vreinterpretq_u32_s32(n),
                vld1q_u32(row.reciprocal[half..].as_ptr()),
            )));
            let step = vld1q_s32(row.step[half..].as_ptr());
            let rem = vshlq_n_s32::<3>(vsubq_s32(n, vmulq_s32(k, step)));
            let five = vmulq_n_s32(step, 5);
            let up = vorrq_u32(
                vcgtq_s32(rem, five),
                vandq_u32(vceqq_s32(rem, five), vld1q_u32(row.tie[half..].as_ptr())),
            );
            let magnitude = vsubq_s32(k, vreinterpretq_s32_u32(up));
            let sign = vshrq_n_s32::<31>(c);
            vsubq_s32(veorq_s32(magnitude, sign), sign)
        }
    }

    pub(super) fn block(samples: &[u8], stride: usize) -> [i32; 64] {
        unsafe {
            let rows: [int16x8_t; 8] = std::array::from_fn(|r| {
                let line = vld1_u8(samples[r * stride..r * stride + 8].as_ptr());
                vsubq_s16(vreinterpretq_s16_u16(vmovl_u8(line)), vdupq_n_s16(128))
            });
            let first = fdct::<false, 12>(transpose(rows));
            let columns = fdct::<true, 14>(transpose(first));
            let mut levels = [0i32; 64];
            let mut lanes = [0i32; 8];
            for (v, row) in columns.iter().enumerate() {
                let low = quantise(vmovl_s16(vget_low_s16(*row)), &ROWS[v], 0);
                let high = quantise(vmovl_high_s16(*row), &ROWS[v], 4);
                vst1q_s32(lanes.as_mut_ptr(), low);
                vst1q_s32(lanes[4..].as_mut_ptr(), high);
                for (u, &value) in lanes.iter().enumerate() {
                    levels[NATURAL_TO_ZIGZAG[v * 8 + u]] = value;
                }
            }
            levels[0] = (i32::from(vgetq_lane_s16::<0>(columns[0])) + 32) >> 6;
            levels
        }
    }
}

/// ffmpeg's `jfdctint` butterfly on the eight columns of `d` at once: row `k` holds sample `k` of
/// each one-dimensional transform. `SECOND` is the column pass, which descales by the pass-one
/// scale as well.
#[inline(always)]
fn fdct<const SECOND: bool>(d: &Block) -> Block {
    let shift = if SECOND { 14 } else { 12 };
    let descale = |x: i32, n: u32| (x + (1 << (n - 1))) >> n;
    let mut o = [[0i32; 8]; 8];
    for i in 0..8 {
        let (t0, t7) = (d[0][i] + d[7][i], d[0][i] - d[7][i]);
        let (t1, t6) = (d[1][i] + d[6][i], d[1][i] - d[6][i]);
        let (t2, t5) = (d[2][i] + d[5][i], d[2][i] - d[5][i]);
        let (t3, t4) = (d[3][i] + d[4][i], d[3][i] - d[4][i]);
        let (t10, t13, t11, t12) = (t0 + t3, t0 - t3, t1 + t2, t1 - t2);
        if SECOND {
            o[0][i] = descale(t10 + t11, 1);
            o[4][i] = descale(t10 - t11, 1);
        } else {
            o[0][i] = (t10 + t11) << 1;
            o[4][i] = (t10 - t11) << 1;
        }
        let z1 = (t12 + t13) * 4433;
        o[2][i] = descale(z1 + t13 * 6270, shift);
        o[6][i] = descale(z1 + t12 * -15137, shift);
        let (t10, t11, t12, t13) = (t4 + t7, t5 + t6, t4 + t6, t5 + t7);
        let z5 = (t12 + t13) * 9633;
        let (t4, t5, t6, t7) = (t4 * 2446, t5 * 16819, t6 * 25172, t7 * 12299);
        let (z1, z2) = (t10 * -7373, t11 * -20995);
        let (z3, z4) = (t12 * -16069 + z5, t13 * -3196 + z5);
        o[7][i] = descale(t4 + z1 + z3, shift);
        o[5][i] = descale(t5 + z2 + z4, shift);
        o[3][i] = descale(t6 + z2 + z3, shift);
        o[1][i] = descale(t7 + z1 + z4, shift);
    }
    o
}

/// ffmpeg's AC quantiser for row `v` of a block: bias 3/8, with ties rounded up only where the
/// step is 4 or 8.
#[inline(always)]
fn quantise(c: &[i32; 8], v: usize) -> [i32; 8] {
    let mut out = [0; 8];
    for u in 0..8 {
        let i = v * 8 + u;
        let (n, q) = (c[u].abs(), QUANT[i]);
        let step = 8 * q;
        let k = ((n as u32 * RECIPROCAL[i]) >> 22) as i32;
        let rem = 8 * (n - k * step);
        let up = (rem > 5 * step) | ((rem == 5 * step) & ((q == 4) | (q == 8)));
        let magnitude = k + i32::from(up);
        out[u] = if c[u] < 0 { -magnitude } else { magnitude };
    }
    out
}

#[derive(Default)]
struct Bits {
    out: Vec<u8>,
    acc: u64,
    count: u32,
}

impl Bits {
    fn put(&mut self, code: u32, length: u32) {
        self.acc = (self.acc << length) | u64::from(code);
        self.count += length;
        if self.count < 32 {
            return;
        }
        self.count -= 32;
        let word = (self.acc >> self.count) as u32;
        if (word & 0x8080_8080) & !word.wrapping_add(0x0101_0101) == 0 {
            self.out.extend(word.to_be_bytes());
        } else {
            for byte in word.to_be_bytes() {
                self.out.push(byte);
                if byte == 0xff {
                    self.out.push(0);
                }
            }
        }
        self.acc &= (1 << self.count) - 1;
    }

    fn finish(mut self) -> Vec<u8> {
        let pad = (8 - self.count % 8) % 8;
        self.acc = (self.acc << pad) | ((1 << pad) - 1);
        self.count += pad;
        while self.count > 0 {
            self.count -= 8;
            let byte = (self.acc >> self.count) as u8;
            self.out.push(byte);
            if byte == 0xff {
                self.out.push(0);
            }
        }
        self.out
    }
}

struct Coder {
    dc: Codes,
    ac: Codes,
}

impl Coder {
    fn magnitude(value: i32) -> (u32, u32) {
        let size = 32 - value.unsigned_abs().leading_zeros();
        let bits = if value < 0 { value - 1 } else { value } as u32 & ((1 << size) - 1);
        (size, bits)
    }

    fn symbol(codes: &Codes, symbol: usize, size: u32, extra: u32, bits: &mut Bits) {
        let (code, length) = codes.0[symbol];
        bits.put((u32::from(code) << size) | extra, u32::from(length) + size);
    }

    /// Writes a block given in zigzag order.
    fn block(&self, bits: &mut Bits, levels: &[i32; 64], predictor: &mut i32) {
        let (size, extra) = Coder::magnitude(levels[0] - *predictor);
        *predictor = levels[0];
        Coder::symbol(&self.dc, size as usize, size, extra, bits);
        let mut nonzero = levels
            .iter()
            .enumerate()
            .skip(1)
            .fold(0u64, |mask, (i, &value)| {
                mask | (u64::from(value != 0) << i)
            });
        let mut last = 0;
        while nonzero != 0 {
            let at = nonzero.trailing_zeros();
            nonzero &= nonzero - 1;
            let mut run = at - last - 1;
            while run > 15 {
                Coder::symbol(&self.ac, 0xf0, 0, 0, bits);
                run -= 16;
            }
            let (size, extra) = Coder::magnitude(levels[at as usize]);
            Coder::symbol(&self.ac, (run << 4 | size) as usize, size, extra, bits);
            last = at;
        }
        if last < 63 {
            Coder::symbol(&self.ac, 0, 0, 0, bits);
        }
    }
}

/// The JPEG file ffmpeg writes for a frame of `width` x `height` given as tightly packed NV12
/// (chroma rows of `2 * ceil(width / 2)` bytes) in the clip's matrix and range.
pub fn encode(nv12: &[u8], width: u32, height: u32, bt709: bool, full: bool) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let source = Source {
        nv12,
        width: w,
        height: h,
        chroma_row: w.div_ceil(2) * 2,
        k: Conversion::new(bt709, full),
    };
    let (cw, ch) = (w / 2, h / 2);
    let mut luma = vec![0u8; w * h];
    let mut cb = vec![0u8; cw * ch];
    let mut cr = vec![0u8; cw * ch];
    let band = ch.div_ceil(THREADS).max(1);
    thread::scope(|scope| {
        let luma_bands = luma.chunks_mut(2 * band * w);
        let chroma_bands = cb.chunks_mut(band * cw).zip(cr.chunks_mut(band * cw));
        for (index, (luma, (cb, cr))) in luma_bands.zip(chroma_bands).enumerate() {
            let source = &source;
            scope.spawn(move || {
                let rows = luma.chunks_mut(2 * w);
                for (row, (luma, (cb, cr))) in rows
                    .zip(cb.chunks_mut(cw).zip(cr.chunks_mut(cw)))
                    .enumerate()
                {
                    let y = 2 * (index * band + row);
                    let (even, odd) = luma.split_at_mut(w);
                    source.luma_row(y, even);
                    source.chroma_row(y + 1, odd, cb, cr);
                }
            });
        }
    });
    if h % 2 == 1 {
        source.luma_row(h - 1, &mut luma[(h - 1) * w..]);
    }
    let luma = Plane {
        samples: luma,
        width: w,
        height: h,
    };
    let cb = Plane {
        samples: cb,
        width: cw,
        height: ch,
    };
    let cr = Plane {
        samples: cr,
        width: cw,
        height: ch,
    };

    let (mcus_x, mcus_y) = (w.div_ceil(16), h.div_ceil(16));
    let luma_coder = Coder {
        dc: Codes::of(&DC_LUMA),
        ac: Codes::of(&AC_LUMA),
    };
    let chroma_coder = Coder {
        dc: Codes::of(&DC_CHROMA),
        ac: Codes::of(&AC_CHROMA),
    };
    let row = |my: usize| {
        let mut bits = Bits::default();
        let mut predictors = [0i32; 3];
        for mx in 0..mcus_x {
            for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let levels = luma.block(2 * mx + dx, 2 * my + dy);
                luma_coder.block(&mut bits, &levels, &mut predictors[0]);
            }
            chroma_coder.block(&mut bits, &cb.block(mx, my), &mut predictors[1]);
            chroma_coder.block(&mut bits, &cr.block(mx, my), &mut predictors[2]);
        }
        bits.finish()
    };
    let mut rows: Vec<Vec<u8>> = vec![Vec::new(); mcus_y];
    let band = mcus_y.div_ceil(THREADS).max(1);
    thread::scope(|scope| {
        for (index, chunk) in rows.chunks_mut(band).enumerate() {
            let row = &row;
            scope.spawn(move || {
                for (offset, out) in chunk.iter_mut().enumerate() {
                    *out = row(index * band + offset);
                }
            });
        }
    });

    let mut file = vec![0xff, 0xd8];
    let mut segment = |marker: u8, body: &[u8]| {
        file.extend([0xff, marker]);
        file.extend(((body.len() + 2) as u16).to_be_bytes());
        file.extend(body);
    };
    segment(0xe0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
    let mut dqt = vec![0u8];
    dqt.extend(ZIGZAG.iter().map(|&natural| QUANT[natural] as u8));
    segment(0xdb, &dqt);
    let mut sof = vec![8];
    sof.extend((height as u16).to_be_bytes());
    sof.extend((width as u16).to_be_bytes());
    sof.extend([3, 1, 0x22, 0, 2, 0x11, 0, 3, 0x11, 0]);
    segment(0xc0, &sof);
    let mut dht = Vec::new();
    for (class, table) in [
        (0x00, &DC_LUMA),
        (0x10, &AC_LUMA),
        (0x01, &DC_CHROMA),
        (0x11, &AC_CHROMA),
    ] {
        dht.push(class);
        dht.extend(table.bits);
        dht.extend(table.values);
    }
    segment(0xc4, &dht);
    segment(0xdd, &(mcus_x as u16).to_be_bytes());
    segment(0xda, &[3, 1, 0x00, 2, 0x11, 3, 0x11, 0, 63, 0]);
    for (index, bytes) in rows.iter().enumerate() {
        if index > 0 {
            file.extend([0xff, 0xd0 + ((index - 1) % 8) as u8]);
        }
        file.extend(bytes);
    }
    file.extend([0xff, 0xd9]);
    file
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: usize, height: usize, seed: u32) -> Vec<u8> {
        let chroma_row = width.div_ceil(2) * 2;
        let mut state = seed;
        let mut noise = move || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 24) as u8
        };
        let mut nv12 = vec![0u8; width * height + chroma_row * height.div_ceil(2)];
        for y in 0..height {
            for x in 0..width {
                let smooth = ((x * 255 / width + y * 255 / height) / 2) as u8;
                nv12[y * width + x] = if (x / 64 + y / 64) % 3 == 0 {
                    noise()
                } else {
                    smooth
                };
            }
        }
        for (i, sample) in nv12[width * height..].iter_mut().enumerate() {
            *sample = if (i / 97) % 2 == 0 {
                noise()
            } else {
                (i % 256) as u8
            };
        }
        nv12
    }

    fn digest(bytes: &[u8]) -> String {
        ring::digest::digest(&ring::digest::SHA256, bytes)
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    #[test]
    fn files_are_unchanged() {
        for (width, height, bt709, full, seed, expected) in [
            (
                1920,
                1080,
                true,
                false,
                1,
                "a251e9541e1dcf5b619c0f28b19867e7027d2ca3339173edc83637885c769045",
            ),
            (
                641,
                359,
                false,
                true,
                2,
                "c5da99e0122bef250707c34bc51621b19500d1ce22a1aaf1d07649840d9d420c",
            ),
            (
                1280,
                720,
                false,
                false,
                3,
                "40fbc66622f50ac421c799b830a5ff67de4559422c11ac6ff6b3238aeefebf4e",
            ),
        ] {
            let file = encode(
                &frame(width, height, seed),
                width as u32,
                height as u32,
                bt709,
                full,
            );
            assert_eq!(digest(&file), expected, "{width}x{height}");
        }
    }

    #[test]
    fn reciprocal_quantisation_equals_division() {
        for i in 1..64 {
            let step = 8 * QUANT[i];
            for n in 0..1 << 15 {
                assert_eq!(
                    ((n as u32 * RECIPROCAL[i]) >> 22) as i32,
                    n / step,
                    "n {n} q {}",
                    QUANT[i]
                );
            }
        }
    }
}
