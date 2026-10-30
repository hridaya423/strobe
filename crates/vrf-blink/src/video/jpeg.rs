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

const RECIPROCAL: [u64; 64] = {
    let mut table = [0; 64];
    let mut i = 0;
    while i < 64 {
        table[i] = (1 << 32) / (8 * QUANT[i] as u64) + 1;
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
    base: [i64; 256],
    red: [i64; 256],
    green_v: [i64; 256],
    green_u: [i64; 256],
    blue: [i64; 256],
}

impl Source<'_> {
    fn new(nv12: &[u8], width: usize, height: usize, k: Conversion) -> Source<'_> {
        let table = |f: &dyn Fn(i64) -> i64| std::array::from_fn(|i| f(i as i64));
        let chroma = |c: i64| (c - 128) << 9;
        Source {
            nv12,
            width,
            height,
            chroma_row: width.div_ceil(2) * 2,
            base: table(&|y| ((y << 9) - i64::from(k.offset)) * i64::from(k.luma) + 2_097_152),
            red: table(&|v| chroma(v) * i64::from(k.red)),
            green_v: table(&|v| chroma(v) * i64::from(k.green_v)),
            green_u: table(&|u| chroma(u) * i64::from(k.green_u)),
            blue: table(&|u| chroma(u) * i64::from(k.blue)),
        }
    }

    fn row(&self, y: usize, out: &mut [[i32; 3]]) {
        let luma = &self.nv12[y * self.width..(y + 1) * self.width];
        let at = self.width * self.height + (y >> 1) * self.chroma_row;
        let chroma = &self.nv12[at..at + self.chroma_row];
        let level = |value: i64| (value >> 22).clamp(0, 255) as i32;
        for (x, pixel) in out.iter_mut().enumerate() {
            let base = self.base[luma[x] as usize];
            let (u, v) = (chroma[x & !1] as usize, chroma[x | 1] as usize);
            *pixel = [
                level(base + self.red[v]),
                level(base + self.green_v[v] + self.green_u[u]),
                level(base + self.blue[u]),
            ];
        }
    }
}

struct Plane {
    samples: Vec<u8>,
    width: usize,
    height: usize,
}

impl Plane {
    fn block(&self, bx: usize, by: usize) -> [i32; 64] {
        let mut rows = [[0i32; 8]; 8];
        for (r, row) in rows.iter_mut().enumerate() {
            let y = (by * 8 + r).min(self.height - 1);
            let line = &self.samples[y * self.width..(y + 1) * self.width];
            *row = fdct(
                std::array::from_fn(|j| i32::from(line[(bx * 8 + j).min(self.width - 1)]) - 128),
                false,
            );
        }
        let mut levels = [0i32; 64];
        for column in 0..8 {
            let c = fdct(rows.map(|row| row[column]), true);
            for (r, value) in c.into_iter().enumerate() {
                let i = r * 8 + column;
                levels[i] = level(value, i);
            }
        }
        levels
    }
}

fn descale(x: i32, n: u32) -> i32 {
    (x + (1 << (n - 1))) >> n
}

fn fdct(d: [i32; 8], second: bool) -> [i32; 8] {
    let (t0, t7) = (d[0] + d[7], d[0] - d[7]);
    let (t1, t6) = (d[1] + d[6], d[1] - d[6]);
    let (t2, t5) = (d[2] + d[5], d[2] - d[5]);
    let (t3, t4) = (d[3] + d[4], d[3] - d[4]);
    let (t10, t13, t11, t12) = (t0 + t3, t0 - t3, t1 + t2, t1 - t2);
    let shift = if second { 14 } else { 12 };
    let mut o = [0i32; 8];
    o[0] = if second {
        descale(t10 + t11, 1)
    } else {
        (t10 + t11) << 1
    };
    o[4] = if second {
        descale(t10 - t11, 1)
    } else {
        (t10 - t11) << 1
    };
    let z1 = (t12 + t13) * 4433;
    o[2] = descale(z1 + t13 * 6270, shift);
    o[6] = descale(z1 + t12 * -15137, shift);
    let (t10, t11, t12, t13) = (t4 + t7, t5 + t6, t4 + t6, t5 + t7);
    let z5 = (t12 + t13) * 9633;
    let (t4, t5, t6, t7) = (t4 * 2446, t5 * 16819, t6 * 25172, t7 * 12299);
    let (z1, z2) = (t10 * -7373, t11 * -20995);
    let (z3, z4) = (t12 * -16069 + z5, t13 * -3196 + z5);
    o[7] = descale(t4 + z1 + z3, shift);
    o[5] = descale(t5 + z2 + z4, shift);
    o[3] = descale(t6 + z2 + z3, shift);
    o[1] = descale(t7 + z1 + z4, shift);
    o
}

fn level(c: i32, i: usize) -> i32 {
    if i == 0 {
        return (c + 32) >> 6;
    }
    let (n, q) = (c.abs(), QUANT[i]);
    let step = 8 * q;
    let k = ((n as u64 * RECIPROCAL[i]) >> 32) as i32;
    let rem = 8 * (n - k * step);
    let up = rem > 5 * step || (rem == 5 * step && (q == 4 || q == 8));
    c.signum() * (k + i32::from(up))
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
        while self.count >= 8 {
            self.count -= 8;
            let byte = (self.acc >> self.count) as u8;
            self.out.push(byte);
            if byte == 0xff {
                self.out.push(0);
            }
        }
        self.acc &= (1 << self.count) - 1;
    }

    fn finish(mut self) -> Vec<u8> {
        if self.count > 0 {
            let pad = 8 - self.count;
            self.put((1 << pad) - 1, pad);
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

    fn block(&self, bits: &mut Bits, levels: &[i32; 64], predictor: &mut i32) {
        let (size, extra) = Coder::magnitude(levels[0] - *predictor);
        *predictor = levels[0];
        let (code, length) = self.dc.0[size as usize];
        bits.put(code.into(), length.into());
        bits.put(extra, size);
        let mut run = 0;
        for &natural in &ZIGZAG[1..] {
            let value = levels[natural];
            if value == 0 {
                run += 1;
                continue;
            }
            while run > 15 {
                let (code, length) = self.ac.0[0xf0];
                bits.put(code.into(), length.into());
                run -= 16;
            }
            let (size, extra) = Coder::magnitude(value);
            let (code, length) = self.ac.0[(run << 4 | size) as usize];
            bits.put(code.into(), length.into());
            bits.put(extra, size);
            run = 0;
        }
        if run > 0 {
            let (code, length) = self.ac.0[0];
            bits.put(code.into(), length.into());
        }
    }
}

/// The JPEG file ffmpeg writes for a frame of `width` x `height` given as tightly packed NV12
/// (chroma rows of `2 * ceil(width / 2)` bytes) in the clip's matrix and range.
pub fn encode(nv12: &[u8], width: u32, height: u32, bt709: bool, full: bool) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let source = Source::new(nv12, w, h, Conversion::new(bt709, full));
    let (cw, ch) = (w / 2, h / 2);
    let mut luma = vec![0u8; w * h];
    let mut cb = vec![0u8; cw * ch];
    let mut cr = vec![0u8; cw * ch];
    let to_luma =
        |[r, g, b]: [i32; 3]| ((9798 * r + 19235 * g + 3735 * b + 16383) >> 15).clamp(0, 255) as u8;
    let band = ch.div_ceil(THREADS).max(1);
    thread::scope(|scope| {
        let luma_bands = luma.chunks_mut(2 * band * w);
        let chroma_bands = cb.chunks_mut(band * cw).zip(cr.chunks_mut(band * cw));
        for (index, (luma, (cb, cr))) in luma_bands.zip(chroma_bands).enumerate() {
            let source = &source;
            scope.spawn(move || {
                let (mut even, mut odd) = (vec![[0i32; 3]; w], vec![[0i32; 3]; w]);
                for (row, (cb, cr)) in cb.chunks_mut(cw).zip(cr.chunks_mut(cw)).enumerate() {
                    let y = 2 * (index * band + row);
                    source.row(y, &mut even);
                    source.row(y + 1, &mut odd);
                    for (out, &pixel) in luma[2 * row * w..(2 * row + 2) * w]
                        .iter_mut()
                        .zip(even.iter().chain(&odd))
                    {
                        *out = to_luma(pixel);
                    }
                    for x in 0..cw {
                        let (a, b) = (odd[2 * x], odd[(2 * x + 1).min(w - 1)]);
                        let s = [a[0] + b[0], a[1] + b[1], a[2] + b[2]];
                        cb[x] = ((-5537 * s[0] - 10848 * s[1] + 16385 * s[2] + 8_388_608) >> 16)
                            .clamp(0, 255) as u8;
                        cr[x] = ((16386 * s[0] - 13730 * s[1] - 2656 * s[2] + 8_388_608) >> 16)
                            .clamp(0, 255) as u8;
                    }
                }
            });
        }
    });
    if h % 2 == 1 {
        let mut last = vec![[0i32; 3]; w];
        source.row(h - 1, &mut last);
        for (out, &pixel) in luma[(h - 1) * w..].iter_mut().zip(&last) {
            *out = to_luma(pixel);
        }
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

    #[test]
    fn reciprocal_quantisation_equals_division() {
        for i in 1..64 {
            let step = 8 * QUANT[i];
            for n in 0..1 << 17 {
                assert_eq!(
                    ((n as u64 * RECIPROCAL[i]) >> 32) as i32,
                    n / step,
                    "n {n} q {}",
                    QUANT[i]
                );
            }
        }
    }
}
