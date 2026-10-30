use vrf_encode::{ColourSpace, Matrix, Range};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Nv12,
    P010,
}

impl Format {
    pub fn bits(self) -> u32 {
        match self {
            Format::Nv12 => 8,
            Format::P010 => 10,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chroma {
    Swscale,
    Spec,
    Exact(ColourSpace),
}

/// Luma weights over `d`, the smallest integers that give the standard's decimal weights.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Weights {
    pub kr: u32,
    pub kg: u32,
    pub kb: u32,
    pub d: u32,
}

impl Weights {
    pub fn of(matrix: Matrix) -> Weights {
        match matrix {
            Matrix::Bt601 => Weights {
                kr: 299,
                kg: 587,
                kb: 114,
                d: 1000,
            },
            Matrix::Bt709 => Weights {
                kr: 1063,
                kg: 3576,
                kb: 361,
                d: 5000,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

type Rgb = [i64; 3];

fn luma_exact(c: Rgb, space: ColourSpace) -> i64 {
    let k = Weights::of(space.matrix);
    let w = i64::from(k.kr) * c[0] + i64::from(k.kg) * c[1] + i64::from(k.kb) * c[2];
    let (scale, offset) = match space.range {
        Range::Limited => (219, 16),
        Range::Full => (255, 0),
    };
    let den = 255 * i64::from(k.d);
    offset + (2 * scale * w + den) / (2 * den)
}

fn chroma_exact(block: [[Rgb; 2]; 2], space: ColourSpace) -> [i64; 2] {
    let k = Weights::of(space.matrix);
    let (kr, kg, kb, d) = (
        i64::from(k.kr),
        i64::from(k.kg),
        i64::from(k.kb),
        i64::from(k.d),
    );
    let mut s = [0i64; 3];
    for c in block.iter().flatten() {
        for (sum, v) in s.iter_mut().zip(c) {
            *sum += v;
        }
    }
    let (scale, top) = match space.range {
        Range::Limited => (224, 240),
        Range::Full => (255, 255),
    };
    let level = |t: i64, base: i64| {
        let den = 2040 * base;
        ((scale * t + den / 2).div_euclid(den) + 128).min(top)
    };
    [
        level((d - kb) * s[2] - kr * s[0] - kg * s[1], d - kb),
        level((d - kr) * s[0] - kg * s[1] - kb * s[2], d - kr),
    ]
}

fn luma_swscale(c: Rgb, bits: u32) -> i64 {
    let v = (5983 * c[0] + 20127 * c[1] + 2032 * c[2] + (0x801 << 8)) >> 9;
    let shift = 14 - bits;
    (v + (1 << (shift - 1))) >> shift
}

fn luma_spec(c: Rgb, bits: u32) -> i64 {
    let m = 1 << (bits - 8);
    (219 * m * (2126 * c[0] + 7152 * c[1] + 722 * c[2]) + 16 * m * 2_550_000 + 1_275_000)
        / 2_550_000
}

fn chroma_pair_swscale(a: Rgb, b: Rgb) -> [i64; 2] {
    let s = [a[0] + b[0], a[1] + b[1], a[2] + b[2]];
    let cb = -3298 * s[0] - 11094 * s[1] + 14392 * s[2];
    let cr = 14392 * s[0] - 13073 * s[1] - 1320 * s[2];
    [
        (cb + (128 << 16) + 512) >> 10,
        (cr + (128 << 16) + 512) >> 10,
    ]
}

fn chroma_swscale(block: [[Rgb; 2]; 2], bits: u32) -> [i64; 2] {
    let top = chroma_pair_swscale(block[0][0], block[0][1]);
    let bottom = chroma_pair_swscale(block[1][0], block[1][1]);
    let shift = 15 - bits;
    let round = 1 << (shift - 1);
    [
        (top[0] + bottom[0] + round) >> shift,
        (top[1] + bottom[1] + round) >> shift,
    ]
}

fn chroma_spec(rows: [[Rgb; 3]; 2], bits: u32) -> [i64; 2] {
    let m = 1 << (bits - 8);
    let mut acc = [0i64; 2];
    for row in rows {
        for (c, weight) in row.into_iter().zip([1, 2, 1]) {
            acc[0] += weight
                * (224 * m * (-2126 * c[0] - 7152 * c[1] + 9278 * c[2]) + 128 * m * 255 * 18556);
            acc[1] += weight
                * (224 * m * (7874 * c[0] - 7152 * c[1] - 722 * c[2]) + 128 * m * 255 * 15748);
        }
    }
    let (db, dr) = (255 * 18556 * 8, 255 * 15748 * 8);
    [(acc[0] + db / 2) / db, (acc[1] + dr / 2) / dr]
}

/// Converts the `rect` crop of an RGBA8 image `stride` pixels wide to
/// 4:2:0, BT.709 limited range unless `chroma` is `Exact`, the CPU twin of the GPU convert pass.
/// Returns the Y plane followed by the interleaved CbCr plane, tightly packed. P010 samples are
/// little-endian `u16` holding the 10-bit value shifted left by 6.
pub fn convert(rgba: &[u8], stride: u32, rect: Rect, chroma: Chroma, format: Format) -> Vec<u8> {
    let bits = format.bits();
    assert!(
        bits == 8 || !matches!(chroma, Chroma::Exact(_)),
        "exact conversion is for 8-bit planes"
    );
    let pixel = |x: u32, y: u32| -> Rgb {
        let i = (((rect.y + y) * stride + rect.x + x) * 4) as usize;
        [rgba[i].into(), rgba[i + 1].into(), rgba[i + 2].into()]
    };
    let mut y_plane = Vec::new();
    let mut cbcr_plane = Vec::new();
    let put = |plane: &mut Vec<u8>, v: i64| {
        if bits == 8 {
            plane.push(v as u8);
        } else {
            plane.extend(((v as u16) << 6).to_le_bytes());
        }
    };
    for y in 0..rect.height {
        for x in 0..rect.width {
            let c = pixel(x, y);
            put(
                &mut y_plane,
                match chroma {
                    Chroma::Swscale => luma_swscale(c, bits),
                    Chroma::Spec => luma_spec(c, bits),
                    Chroma::Exact(space) => luma_exact(c, space),
                },
            );
        }
    }
    for cy in 0..rect.height / 2 {
        for cx in 0..rect.width / 2 {
            let (x0, y0) = (cx * 2, cy * 2);
            let [cb, cr] = match chroma {
                Chroma::Swscale => chroma_swscale(
                    [
                        [pixel(x0, y0), pixel(x0 + 1, y0)],
                        [pixel(x0, y0 + 1), pixel(x0 + 1, y0 + 1)],
                    ],
                    bits,
                ),
                Chroma::Exact(space) => chroma_exact(
                    [
                        [pixel(x0, y0), pixel(x0 + 1, y0)],
                        [pixel(x0, y0 + 1), pixel(x0 + 1, y0 + 1)],
                    ],
                    space,
                ),
                Chroma::Spec => {
                    let row = |y| {
                        [
                            pixel(x0.saturating_sub(1), y),
                            pixel(x0, y),
                            pixel(x0 + 1, y),
                        ]
                    };
                    chroma_spec([row(y0), row(y0 + 1)], bits)
                }
            };
            put(&mut cbcr_plane, cb);
            put(&mut cbcr_plane, cr);
        }
    }
    y_plane.extend(cbcr_plane);
    y_plane
}
