#[allow(dead_code)]
mod gpu_support;

use std::path::PathBuf;
use std::process::Command;

use gpu_support::{NV12, P010, Surface};
use vrf::colour::{self, Chroma, Format, Rect};
use vrf::gpu::Gpu;
use vrf::gpu::convert::Converter;

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
            src,
            &y.create_view(&Default::default()),
            &cbcr.create_view(&Default::default()),
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
                    converter.encode(&mut pass, &src, &yv, &cv, rect, chroma);
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
