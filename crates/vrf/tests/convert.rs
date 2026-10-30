#[allow(dead_code)]
mod gpu_support;

use std::path::PathBuf;
use std::process::Command;

#[cfg(windows)]
use gpu_support::D3d11;
#[cfg(target_os = "macos")]
use gpu_support::{NV12, P010, Surface};
use vrf::colour::{self, Chroma, Format, Rect};
use vrf::gpu::Gpu;
use vrf::gpu::convert::Converter;
use vrf_encode::{ColourSpace, Matrix, Range};

const FORMATS: [Format; 2] = [Format::Nv12, Format::P010];

struct Image {
    name: &'static str,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
}

fn noise() -> Image {
    let (width, height) = (1920, 1080);
    let mut rng = SplitMix(1);
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    for px in rgba.chunks_exact_mut(4) {
        let v = rng.next().to_le_bytes();
        px.copy_from_slice(&[v[0], v[1], v[2], 255]);
    }
    Image {
        name: "noise",
        width,
        height,
        rgba,
    }
}

fn gradient() -> Image {
    let (width, height) = (1920u32, 1080u32);
    let mut rng = SplitMix(2);
    let mut rgba = vec![255u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let i = ((y * width + x) * 4) as usize;
            rgba[i] = (x * 255 / width) as u8;
            rgba[i + 1] = (y * 255 / height) as u8;
            rgba[i + 2] = ((x + y) * 255 / (width + height)) as u8;
            if (200..500).contains(&y) && (300..900).contains(&x) {
                let v = rng.next().to_le_bytes();
                rgba[i..i + 3].copy_from_slice(&v[..3]);
            }
        }
    }
    Image {
        name: "gradient",
        width,
        height,
        rgba,
    }
}

fn all_colours() -> Image {
    let (width, height) = (4096u32, 4096u32);
    let mut rgba = vec![255u8; (width * height * 4) as usize];
    for (i, px) in rgba.chunks_exact_mut(4).enumerate() {
        px[..3].copy_from_slice(&[(i >> 16) as u8, (i >> 8) as u8, i as u8]);
    }
    Image {
        name: "all_colours",
        width,
        height,
        rgba,
    }
}

fn extremes() -> Image {
    let (width, height) = (512, 512);
    let mut rng = SplitMix(3);
    let mut rgba = vec![255u8; (width * height * 4) as usize];
    for px in rgba.chunks_exact_mut(4) {
        let v = rng.next();
        for (c, byte) in px[..3].iter_mut().enumerate() {
            *byte = if v >> c & 1 == 1 { 255 } else { 0 };
        }
    }
    Image {
        name: "extremes",
        width,
        height,
        rgba,
    }
}

struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn tmp(name: &str) -> TempFile {
    TempFile(PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name))
}

fn ffmpeg(image: &Image, rect: Rect, format: Format) -> Vec<u8> {
    let tag = format!("{}-{:?}-{}-{}", image.name, format, rect.x, rect.y);
    let input = tmp(&format!("{tag}.rgba"));
    let output = tmp(&format!("{tag}.raw"));
    std::fs::write(&input.0, &image.rgba).unwrap();
    let crop = format!("crop={}:{}:{}:{}", rect.width, rect.height, rect.x, rect.y);
    let scale = "scale=out_color_matrix=bt709:out_range=tv:flags=area+accurate_rnd+full_chroma_int";
    let pix_fmt = match format {
        Format::Nv12 => "nv12",
        Format::P010 => "p010le",
    };
    let status = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
        ])
        .args(["-s", &format!("{}x{}", image.width, image.height), "-i"])
        .arg(&input.0)
        .args([
            "-vf",
            &format!("{crop},{scale}"),
            "-pix_fmt",
            pix_fmt,
            "-f",
            "rawvideo",
        ])
        .arg(&output.0)
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::read(&output.0).unwrap()
}

fn input_texture(gpu: &Gpu, image: &Image, bgra: bool) -> wgpu::TextureView {
    let mut bytes = image.rgba.clone();
    if bgra {
        bytes.chunks_exact_mut(4).for_each(|px| px.swap(0, 2));
    }
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: image.width,
            height: image.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: if bgra {
            wgpu::TextureFormat::Bgra8Unorm
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        },
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        texture.as_image_copy(),
        &bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(image.width * 4),
            rows_per_image: None,
        },
        texture.size(),
    );
    texture.create_view(&Default::default())
}

fn plane_texture(gpu: &Gpu, format: wgpu::TextureFormat, width: u32, height: u32) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

fn run_converter(
    gpu: &Gpu,
    converter: &Converter,
    src: &wgpu::TextureView,
    y: &wgpu::Texture,
    cbcr: &wgpu::Texture,
    rect: Rect,
    chroma: Chroma,
) {
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        converter.encode(
            &mut pass,
            &converter.source(src),
            &converter.target(
                &y.create_view(&Default::default()),
                &cbcr.create_view(&Default::default()),
            ),
            rect,
            chroma,
        );
    }
    gpu.queue.submit([encoder.finish()]);
}

fn gpu_convert(
    gpu: &Gpu,
    image: &Image,
    rect: Rect,
    chroma: Chroma,
    format: Format,
    bgra: bool,
) -> Vec<u8> {
    let converter = Converter::new(gpu, format);
    let (yf, cf) = Converter::plane_formats(format);
    let y = plane_texture(gpu, yf, rect.width, rect.height);
    let cbcr = plane_texture(gpu, cf, rect.width / 2, rect.height / 2);
    run_converter(
        gpu,
        &converter,
        &input_texture(gpu, image, bgra),
        &y,
        &cbcr,
        rect,
        chroma,
    );
    let mut out = gpu.read_texture(&y).unwrap();
    out.extend(gpu.read_texture(&cbcr).unwrap());
    out
}

fn full(image: &Image) -> Rect {
    Rect {
        x: 0,
        y: 0,
        width: image.width,
        height: image.height,
    }
}

fn assert_same(what: &str, got: &[u8], want: &[u8]) {
    assert_eq!(got.len(), want.len(), "{what}: length");
    if let Some(i) = got.iter().zip(want).position(|(a, b)| a != b) {
        let wrong = got.iter().zip(want).filter(|(a, b)| a != b).count();
        panic!(
            "{what}: {wrong} bytes differ, first at {i}: got {} want {}",
            got[i], want[i]
        );
    }
}

fn swscale_matches_ffmpeg(image: Image) {
    let gpu = Gpu::new().unwrap();
    for format in FORMATS {
        let rect = full(&image);
        let want = ffmpeg(&image, rect, format);
        let oracle = colour::convert(&image.rgba, image.width, rect, Chroma::Swscale, format);
        assert_same(
            &format!("{} {format:?} oracle vs ffmpeg", image.name),
            &oracle,
            &want,
        );
        let got = gpu_convert(&gpu, &image, rect, Chroma::Swscale, format, false);
        assert_same(
            &format!("{} {format:?} gpu vs ffmpeg", image.name),
            &got,
            &want,
        );
    }
}

#[test]
fn swscale_noise_matches_ffmpeg() {
    swscale_matches_ffmpeg(noise());
}

#[test]
fn swscale_gradient_matches_ffmpeg() {
    swscale_matches_ffmpeg(gradient());
}

#[test]
fn swscale_all_colours_matches_ffmpeg() {
    swscale_matches_ffmpeg(all_colours());
}

#[test]
fn swscale_extremes_matches_ffmpeg() {
    swscale_matches_ffmpeg(extremes());
}

#[test]
fn swscale_crops_in_all_parities_match_ffmpeg_crop() {
    let gpu = Gpu::new().unwrap();
    let image = noise();
    for format in FORMATS {
        for (x, y) in [(100, 50), (101, 50), (100, 51), (101, 51), (3, 479)] {
            let rect = Rect {
                x,
                y,
                width: 1000,
                height: 600,
            };
            let want = ffmpeg(&image, rect, format);
            let oracle = colour::convert(&image.rgba, image.width, rect, Chroma::Swscale, format);
            assert_same(&format!("{format:?} crop {x},{y} oracle"), &oracle, &want);
            let got = gpu_convert(&gpu, &image, rect, Chroma::Swscale, format, false);
            assert_same(&format!("{format:?} crop {x},{y} gpu"), &got, &want);
        }
    }
}

#[test]
fn bgra_input_gives_the_same_bytes_as_rgba() {
    let gpu = Gpu::new().unwrap();
    let image = gradient();
    let rect = Rect {
        x: 7,
        y: 9,
        width: 800,
        height: 400,
    };
    for format in FORMATS {
        for chroma in [Chroma::Swscale, Chroma::Spec] {
            let rgba = gpu_convert(&gpu, &image, rect, chroma, format, false);
            let bgra = gpu_convert(&gpu, &image, rect, chroma, format, true);
            assert_same(&format!("{format:?} {chroma:?}"), &bgra, &rgba);
        }
    }
}

#[test]
fn spec_gpu_matches_oracle() {
    let gpu = Gpu::new().unwrap();
    for image in [noise(), extremes(), all_colours()] {
        for format in FORMATS {
            for (x, y) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let rect = Rect {
                    x,
                    y,
                    width: image.width - 2,
                    height: image.height - 2,
                };
                let want = colour::convert(&image.rgba, image.width, rect, Chroma::Spec, format);
                let got = gpu_convert(&gpu, &image, rect, Chroma::Spec, format, false);
                assert_same(
                    &format!("{} {format:?} crop {x},{y}", image.name),
                    &got,
                    &want,
                );
            }
        }
    }
}

const SPACES: [ColourSpace; 4] = [
    ColourSpace {
        matrix: Matrix::Bt601,
        range: Range::Limited,
    },
    ColourSpace {
        matrix: Matrix::Bt601,
        range: Range::Full,
    },
    ColourSpace {
        matrix: Matrix::Bt709,
        range: Range::Limited,
    },
    ColourSpace {
        matrix: Matrix::Bt709,
        range: Range::Full,
    },
];

#[test]
fn exact_gpu_matches_oracle() {
    let gpu = Gpu::new().unwrap();
    for image in [noise(), gradient(), extremes(), all_colours()] {
        for space in SPACES {
            for (x, y) in [(0, 0), (1, 1)] {
                let rect = Rect {
                    x,
                    y,
                    width: image.width - 2,
                    height: image.height - 2,
                };
                let chroma = Chroma::Exact(space);
                let want = colour::convert(&image.rgba, image.width, rect, chroma, Format::Nv12);
                let got = gpu_convert(&gpu, &image, rect, chroma, Format::Nv12, false);
                assert_same(
                    &format!("{} {space:?} crop {x},{y}", image.name),
                    &got,
                    &want,
                );
            }
        }
    }
}

#[test]
fn exact_planes_are_the_nearest_integers_of_the_definition() {
    let mut rng = SplitMix(7);
    let (blocks_x, blocks_y) = (512u32, 512u32);
    let (width, height) = (blocks_x * 2, blocks_y * 2);
    let mut rgba = vec![255u8; (width * height * 4) as usize];
    let mut colours = Vec::new();
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let v = rng.next().to_le_bytes();
            colours.push([v[0], v[1], v[2]]);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let i = (((by * 2 + dy) * width + bx * 2 + dx) * 4) as usize;
                rgba[i..i + 3].copy_from_slice(&v[..3]);
            }
        }
    }
    let rect = Rect {
        x: 0,
        y: 0,
        width,
        height,
    };
    for space in SPACES {
        let out = colour::convert(&rgba, width, rect, Chroma::Exact(space), Format::Nv12);
        let (luma, chroma) = out.split_at((width * height) as usize);
        let (kr, kb) = match space.matrix {
            Matrix::Bt601 => (0.299, 0.114),
            Matrix::Bt709 => (0.2126, 0.0722),
        };
        let (y_scale, y_off, c_scale) = match space.range {
            Range::Limited => (219.0, 16.0, 224.0),
            Range::Full => (255.0, 0.0, 255.0),
        };
        for (n, c) in colours.iter().enumerate() {
            let (r, g, b) = (f64::from(c[0]), f64::from(c[1]), f64::from(c[2]));
            let y = (kr * r + (1.0 - kr - kb) * g + kb * b) / 255.0;
            let cb = (b / 255.0 - y) / (2.0 * (1.0 - kb));
            let cr = (r / 255.0 - y) / (2.0 * (1.0 - kr));
            let (bx, by) = (n as u32 % blocks_x, n as u32 / blocks_x);
            let got = [
                luma[(by * 2 * width + bx * 2) as usize],
                chroma[((by * blocks_x + bx) * 2) as usize],
                chroma[((by * blocks_x + bx) * 2 + 1) as usize],
            ];
            let want = [
                y_off + y_scale * y,
                (128.0 + c_scale * cb).min(255.0),
                (128.0 + c_scale * cr).min(255.0),
            ];
            for (got, want) in got.into_iter().zip(want) {
                assert!(
                    (f64::from(got) - want).abs() <= 0.5 + 1e-6,
                    "{space:?} {c:?}: got {got} want {want}"
                );
            }
        }
    }
}

fn ffmpeg_decode(planes: &[u8], width: u32, height: u32, space: ColourSpace) -> Vec<u8> {
    let tag = format!("decode-{:?}-{:?}", space.matrix, space.range);
    let input = tmp(&format!("{tag}.nv12"));
    let output = tmp(&format!("{tag}.rgb"));
    std::fs::write(&input.0, planes).unwrap();
    let matrix = match space.matrix {
        Matrix::Bt601 => "bt601",
        Matrix::Bt709 => "bt709",
    };
    let range = match space.range {
        Range::Limited => "tv",
        Range::Full => "pc",
    };
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo"])
        .args(["-pix_fmt", "nv12", "-s", &format!("{width}x{height}"), "-i"])
        .arg(&input.0)
        .args([
            "-vf",
            &format!(
                "scale=in_color_matrix={matrix}:in_range={range}:flags=accurate_rnd+full_chroma_int+bitexact,format=rgb24"
            ),
            "-f",
            "rawvideo",
        ])
        .arg(&output.0)
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::read(&output.0).unwrap()
}

#[test]
fn a_blank_frame_decodes_to_its_colour_when_full_range_bt601() {
    let space = ColourSpace {
        matrix: Matrix::Bt601,
        range: Range::Full,
    };
    let rgb = [243u8, 239, 230];
    let rgba: Vec<u8> = (0..64 * 64)
        .flat_map(|_| [rgb[0], rgb[1], rgb[2], 255])
        .collect();
    let rect = Rect {
        x: 0,
        y: 0,
        width: 64,
        height: 64,
    };
    let planes = colour::convert(&rgba, 64, rect, Chroma::Exact(space), Format::Nv12);
    assert_eq!([planes[0], planes[4096], planes[4097]], [239, 123, 131]);
    let decoded = ffmpeg_decode(&planes, 64, 64, space);
    assert!(decoded.chunks_exact(3).all(|px| px == rgb));
}

#[test]
fn full_range_roundtrips_within_one_level_and_beats_limited() {
    const BLOCK: u32 = 16;
    let size = 2048u32;
    let mut rgba = vec![255u8; (size * size * 4) as usize];
    let mut rng = SplitMix(11);
    let per_row = size / BLOCK;
    for block in 0..per_row * per_row {
        let v = rng.next().to_le_bytes();
        let (bx, by) = (block % per_row, block / per_row);
        for dy in 0..BLOCK {
            for dx in 0..BLOCK {
                let i = (((by * BLOCK + dy) * size + bx * BLOCK + dx) * 4) as usize;
                rgba[i..i + 3].copy_from_slice(&v[..3]);
            }
        }
    }
    let rect = Rect {
        x: 0,
        y: 0,
        width: size,
        height: size,
    };
    let mean_error = |space: ColourSpace| {
        let planes = colour::convert(&rgba, size, rect, Chroma::Exact(space), Format::Nv12);
        let decoded = ffmpeg_decode(&planes, size, size, space);
        let (mut sum, mut count, mut worst) = (0u64, 0u64, 0i32);
        for y in 0..size {
            for x in 0..size {
                if !(4..12).contains(&(x % BLOCK)) || !(4..12).contains(&(y % BLOCK)) {
                    continue;
                }
                let i = (y * size + x) as usize;
                for c in 0..3 {
                    let e = (i32::from(rgba[i * 4 + c]) - i32::from(decoded[i * 3 + c])).abs();
                    sum += e as u64;
                    count += 1;
                    worst = worst.max(e);
                }
            }
        }
        (sum as f64 / count as f64, worst)
    };
    for matrix in [Matrix::Bt601, Matrix::Bt709] {
        let (limited, _) = mean_error(ColourSpace {
            matrix,
            range: Range::Limited,
        });
        let (full, worst) = mean_error(ColourSpace {
            matrix,
            range: Range::Full,
        });
        assert!(worst <= 1, "{matrix:?} full range worst error {worst}");
        assert!(full < limited, "{matrix:?} full {full} limited {limited}");
    }
}

#[test]
fn solid_colours_land_on_the_bt709_limited_range_values() {
    let gpu = Gpu::new().unwrap();
    let cases: [([u8; 3], [i32; 3]); 5] = [
        ([255, 255, 255], [235, 128, 128]),
        ([0, 0, 0], [16, 128, 128]),
        ([255, 0, 0], [63, 102, 240]),
        ([0, 255, 0], [173, 42, 26]),
        ([0, 0, 255], [32, 240, 118]),
    ];
    for (rgb, want) in cases {
        let rgba: Vec<u8> = (0..16 * 16)
            .flat_map(|_| [rgb[0], rgb[1], rgb[2], 255])
            .collect();
        let image = Image {
            name: "solid",
            width: 16,
            height: 16,
            rgba,
        };
        for chroma in [Chroma::Swscale, Chroma::Spec] {
            let out = gpu_convert(&gpu, &image, full(&image), chroma, Format::Nv12, false);
            for (i, (got, want)) in [out[0], out[256], out[257]]
                .into_iter()
                .zip(want)
                .enumerate()
            {
                assert!(
                    (i32::from(got) - want).abs() <= 1,
                    "{rgb:?} {chroma:?} plane {i}: got {got} want {want}"
                );
            }
            let out10 = gpu_convert(&gpu, &image, full(&image), chroma, Format::P010, false);
            let sample =
                |i: usize| i32::from(u16::from_le_bytes([out10[i * 2], out10[i * 2 + 1]]) >> 6);
            for (i, want) in [(0, want[0]), (256, want[1]), (257, want[2])] {
                assert!(
                    (sample(i) - want * 4).abs() <= 4,
                    "{rgb:?} {chroma:?} p010 {i}: got {} want {}",
                    sample(i),
                    want * 4
                );
            }
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn converts_straight_into_iosurface_planes() {
    let gpu = Gpu::new().unwrap();
    let image = noise();
    let rect = Rect {
        x: 101,
        y: 51,
        width: 1280,
        height: 720,
    };
    for (format, pixel_format, bytes) in [(Format::Nv12, NV12, 1usize), (Format::P010, P010, 2)] {
        let (w, h) = (rect.width as usize, rect.height as usize);
        let surface = Surface::new(w, h, pixel_format);
        let (yf, cf) = Converter::plane_formats(format);
        let y = gpu.import_iosurface(&surface.surface, 0, yf).unwrap();
        let cbcr = gpu.import_iosurface(&surface.surface, 1, cf).unwrap();
        let converter = Converter::new(&gpu, format);
        run_converter(
            &gpu,
            &converter,
            &input_texture(&gpu, &image, true),
            &y,
            &cbcr,
            rect,
            Chroma::Swscale,
        );
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();

        let want = colour::convert(&image.rgba, image.width, rect, Chroma::Swscale, format);
        let (want_y, want_c) = want.split_at(w * h * bytes);
        assert_same("iosurface y", &surface.read_plane(0, w * bytes, h), want_y);
        assert_same(
            "iosurface cbcr",
            &surface.read_plane(1, w * bytes, h / 2),
            want_c,
        );
    }
}

#[cfg(windows)]
#[test]
fn converts_an_imported_shared_handle_with_and_without_a_copy() {
    let gpu = Gpu::new().unwrap();
    let d3d11 = D3d11::new(gpu.luid());
    let image = noise();
    let rect = Rect {
        x: 101,
        y: 51,
        width: 1280,
        height: 720,
    };
    let mut bgra = image.rgba.clone();
    bgra.chunks_exact_mut(4).for_each(|px| px.swap(0, 2));
    let shared = d3d11.shared_texture(image.width, image.height);
    d3d11.write(&shared, &bgra);
    let handle = unsafe { vrf::gpu::d3d12::SharedHandle::duplicate(shared.handle()) }.unwrap();
    for format in FORMATS {
        let want = colour::convert(&image.rgba, image.width, rect, Chroma::Swscale, format);
        let converter = Converter::new(&gpu, format);
        let (yf, cf) = Converter::plane_formats(format);
        for direct in [false, true] {
            let usage = if direct {
                wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::TEXTURE_BINDING
            } else {
                wgpu::TextureUsages::COPY_SRC
            };
            let imported = gpu.import_shared_handle(&handle, usage).unwrap();
            let view = if direct {
                imported.create_view(&Default::default())
            } else {
                let owned = gpu.device.create_texture(&wgpu::TextureDescriptor {
                    label: None,
                    size: imported.size(),
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: imported.format(),
                    usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                });
                let mut encoder = gpu.device.create_command_encoder(&Default::default());
                encoder.copy_texture_to_texture(
                    imported.as_image_copy(),
                    owned.as_image_copy(),
                    imported.size(),
                );
                gpu.queue.submit([encoder.finish()]);
                owned.create_view(&Default::default())
            };
            let y = plane_texture(&gpu, yf, rect.width, rect.height);
            let cbcr = plane_texture(&gpu, cf, rect.width / 2, rect.height / 2);
            run_converter(&gpu, &converter, &view, &y, &cbcr, rect, Chroma::Swscale);
            let mut got = gpu.read_texture(&y).unwrap();
            got.extend(gpu.read_texture(&cbcr).unwrap());
            assert_same(&format!("{format:?} direct={direct}"), &got, &want);
        }
    }
}

#[cfg(windows)]
#[test]
fn every_adapter_writes_the_plane_formats_and_converts_exactly() {
    let image = noise();
    let rect = Rect {
        x: 101,
        y: 51,
        width: 1280,
        height: 720,
    };
    for (name, luid) in Gpu::adapters() {
        let gpu = Gpu::with_luid(luid).unwrap();
        for format in FORMATS {
            let (yf, cf) = Converter::plane_formats(format);
            for plane in [yf, cf] {
                let usages = gpu
                    .adapter
                    .get_texture_format_features(plane)
                    .allowed_usages;
                println!("{name} {plane:?}: {usages:?}");
                assert!(
                    usages.contains(wgpu::TextureUsages::STORAGE_BINDING),
                    "{name} {plane:?}"
                );
            }
            let want = colour::convert(&image.rgba, image.width, rect, Chroma::Swscale, format);
            let got = gpu_convert(&gpu, &image, rect, Chroma::Swscale, format, false);
            assert_same(&format!("{name} {format:?}"), &got, &want);
        }
    }
}

#[cfg(windows)]
fn deferred_cost(direct: bool) {
    use std::time::{Duration, Instant};

    const FRAMES: usize = 2000;
    const BUCKET: usize = 250;
    let gpu = Gpu::new().unwrap();
    let d3d11 = D3d11::new(gpu.luid());
    let (w, h) = (1920, 1080);
    let pool: Vec<_> = (0..3).map(|_| d3d11.shared_texture(w, h)).collect();
    let rect = Rect {
        x: 0,
        y: 0,
        width: w,
        height: h,
    };
    let converter = Converter::new(&gpu, Format::Nv12);
    let (yf, cf) = Converter::plane_formats(Format::Nv12);
    let y = plane_texture(&gpu, yf, w, h);
    let cbcr = plane_texture(&gpu, cf, w / 2, h / 2);
    let target = converter.target(
        &y.create_view(&Default::default()),
        &cbcr.create_view(&Default::default()),
    );
    let owned = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8Unorm,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let owned_view = owned.create_view(&Default::default());
    let wait = || {
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
    };
    let mut rows: Vec<[Duration; 5]> = Vec::new();
    for frame in 0..FRAMES {
        d3d11.clear(&pool[frame % pool.len()], [(frame % 251) as u8, 7, 9, 255]);
        let handle =
            unsafe { vrf::gpu::d3d12::SharedHandle::duplicate(pool[frame % pool.len()].handle()) }
                .unwrap();
        let t0 = Instant::now();
        let usage = if direct {
            wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::TEXTURE_BINDING
        } else {
            wgpu::TextureUsages::COPY_SRC
        };
        let imported = gpu.import_shared_handle(&handle, usage).unwrap();
        let t1 = Instant::now();
        if !direct {
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            encoder.copy_texture_to_texture(
                imported.as_image_copy(),
                owned.as_image_copy(),
                imported.size(),
            );
            gpu.queue.submit([encoder.finish()]);
            wait();
        }
        let t2 = Instant::now();
        let source = if direct {
            converter.source(&imported.create_view(&Default::default()))
        } else {
            converter.source(&owned_view)
        };
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            converter.encode(&mut pass, &source, &target, rect, Chroma::Swscale);
        }
        gpu.queue.submit([encoder.finish()]);
        wait();
        let t3 = Instant::now();
        drop(source);
        drop(imported);
        wait();
        let t4 = Instant::now();
        rows.push([t1 - t0, t2 - t1, t3 - t2, t4 - t3, t4 - t0]);
    }
    println!("{} direct={direct}", gpu.adapter.get_info().name,);
    for (i, bucket) in rows.chunks(BUCKET).enumerate() {
        let mean = |k: usize| {
            bucket.iter().map(|r| r[k].as_secs_f64()).sum::<f64>() / bucket.len() as f64 * 1e3
        };
        println!(
            "frames {:>4}..{:>4}: open {:.3} ms, copy {:.3} ms, convert {:.3} ms, release {:.3} ms, total {:.3} ms",
            i * BUCKET,
            i * BUCKET + bucket.len(),
            mean(0),
            mean(1),
            mean(2),
            mean(3),
            mean(4)
        );
    }
}

#[cfg(windows)]
#[test]
#[ignore]
fn deferred_cost_with_a_copy() {
    deferred_cost(false);
}

#[cfg(windows)]
#[test]
#[ignore]
fn deferred_cost_converting_the_opened_resource() {
    deferred_cost(true);
}

#[test]
#[ignore]
fn gpu_time_per_convert() {
    const RUNS: u32 = 200;
    let gpu = Gpu::new().unwrap();
    assert!(
        gpu.device
            .features()
            .contains(wgpu::Features::TIMESTAMP_QUERY)
    );
    for (width, height) in [(1920u32, 1080u32), (3840, 2160)] {
        let mut rng = SplitMix(9);
        let rgba: Vec<u8> = (0..width * height)
            .flat_map(|_| {
                let v = rng.next().to_le_bytes();
                [v[0], v[1], v[2], 255]
            })
            .collect();
        let image = Image {
            name: "bench",
            width,
            height,
            rgba,
        };
        let src = input_texture(&gpu, &image, true);
        for format in FORMATS {
            for chroma in [Chroma::Swscale, Chroma::Spec] {
                let converter = Converter::new(&gpu, format);
                let (yf, cf) = Converter::plane_formats(format);
                let y = plane_texture(&gpu, yf, width, height);
                let cbcr = plane_texture(&gpu, cf, width / 2, height / 2);
                let (yv, cv) = (
                    y.create_view(&Default::default()),
                    cbcr.create_view(&Default::default()),
                );
                let queries = gpu.device.create_query_set(&wgpu::QuerySetDescriptor {
                    label: None,
                    ty: wgpu::QueryType::Timestamp,
                    count: RUNS * 2,
                });
                let resolve = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: u64::from(RUNS * 16),
                    usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                });
                let read = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: u64::from(RUNS * 16),
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                let rect = Rect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                };
                let (source, target) = (converter.source(&src), converter.target(&yv, &cv));
                let mut encoder = gpu.device.create_command_encoder(&Default::default());
                for i in 0..RUNS {
                    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: None,
                        timestamp_writes: Some(wgpu::ComputePassTimestampWrites {
                            query_set: &queries,
                            beginning_of_pass_write_index: Some(i * 2),
                            end_of_pass_write_index: Some(i * 2 + 1),
                        }),
                    });
                    converter.encode(&mut pass, &source, &target, rect, chroma);
                }
                encoder.resolve_query_set(&queries, 0..RUNS * 2, &resolve, 0);
                encoder.copy_buffer_to_buffer(&resolve, 0, &read, 0, u64::from(RUNS * 16));
                gpu.queue.submit([encoder.finish()]);
                read.map_async(wgpu::MapMode::Read, .., |r| r.unwrap());
                gpu.device
                    .poll(wgpu::PollType::wait_indefinitely())
                    .unwrap();
                let ticks: Vec<u64> = read
                    .get_mapped_range(..)
                    .unwrap()
                    .chunks_exact(8)
                    .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
                    .collect();
                let period = f64::from(gpu.queue.get_timestamp_period());
                let mut ms: Vec<f64> = ticks
                    .chunks_exact(2)
                    .map(|t| (t[1] - t[0]) as f64 * period / 1e6)
                    .collect();
                ms.sort_by(f64::total_cmp);
                println!(
                    "BENCH {width}x{height} {format:?} {chroma:?}: median {:.4} ms, min {:.4} ms, p90 {:.4} ms",
                    ms[ms.len() / 2],
                    ms[0],
                    ms[ms.len() * 9 / 10]
                );
            }
        }
    }
}
