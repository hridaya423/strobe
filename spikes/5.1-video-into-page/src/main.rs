mod bundle;
mod decoder;
mod gpu;
mod host;
mod mac;
mod verify;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use host::{Blob, View};
use objc2_io_surface::{IOSurfaceLockOptions, IOSurfaceRef};
use verify::{Layout, Reading};

const FPS: i64 = 30;
const WARMUP: usize = 8;
const PAINT_TIMEOUT: Duration = Duration::from_secs(3);

unsafe extern "C" {
    fn vsp_jpeg(bgra: *const u8, width: usize, height: usize, stride: usize, quality: f64, out: *mut u8, capacity: usize) -> usize;
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    host::run(move |ui| -> Result<()> {
        let options = Options::parse(&args)?;
        match options.command.as_str() {
            "probe" => probe(&ui),
            "decode" => decode_only(&options),
            "run" => run(&ui, &options),
            other => bail!("unknown command {other}"),
        }
    })
}

#[derive(Clone)]
struct Options {
    command: String,
    values: HashMap<String, String>,
}

impl Options {
    fn parse(args: &[String]) -> Result<Options> {
        let mut values = HashMap::new();
        let mut iter = args.iter().skip(1);
        let mut positional = 0;
        while let Some(arg) = iter.next() {
            if let Some(key) = arg.strip_prefix("--") {
                values.insert(key.to_owned(), iter.next().context("option without a value")?.clone());
            } else {
                values.insert(format!("arg{positional}"), arg.clone());
                positional += 1;
            }
        }
        Ok(Options { command: args.first().cloned().unwrap_or_default(), values })
    }

    fn get(&self, key: &str, default: &str) -> String {
        self.values.get(key).cloned().unwrap_or_else(|| default.to_owned())
    }

    fn number(&self, key: &str, default: usize) -> usize {
        self.values.get(key).and_then(|v| v.parse().ok()).unwrap_or(default)
    }
}

fn page(name: &str) -> String {
    format!("file://{}/pages/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn probe(ui: &host::Ui) -> Result<()> {
    let mut view = View::open(ui, &page("probe.html"), 640, 360, false)?;
    let result = view.evaluate("probe()")?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    view.close()
}

fn uptime() -> String {
    let output = std::process::Command::new("uptime").output().unwrap();
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn decode_only(options: &Options) -> Result<()> {
    let clip = options.get("arg0", "");
    let probed = verify::probe(&clip)?;
    println!("uptime: {}", uptime());
    for round in 0..3 {
        let decoded = decoder::decode(&clip, decoder::NV12_VIDEO_RANGE)?;
        if round == 0 {
            let ids: Vec<u64> = decoded.frames.iter().map(|f| luma_id(&f.surface(), probed.width)).collect();
            let sequential = ids.iter().skip(ids.len() - probed.pts.len()).enumerate().all(|(k, id)| *id == k as u64);
            println!("decoder frames {} (ffprobe {}), first ids {:?}, ids match frame order: {sequential}", ids.len(), probed.pts.len(), &ids[..4.min(ids.len())]);
        }
        println!(
            "round {round}: {} frames {}x{} in {:.3} s = {:.1} fps",
            decoded.frames.len(),
            probed.width,
            probed.height,
            decoded.seconds,
            decoded.frames.len() as f64 / decoded.seconds
        );
        if round == 0 {
            let mut values: Vec<(i64, i32)> = decoded.frames.iter().map(|f| (f.value, f.timescale)).collect();
            let sorted = values.windows(2).all(|pair| pair[0].0 <= pair[1].0);
            values.sort();
            let same = values.len() == probed.pts.len()
                && values.iter().zip(&probed.pts).all(|((value, scale), pts)| {
                    (*value as f64 / f64::from(*scale) - probed.seconds(*pts)).abs() < 1e-6
                });
            println!("pts in presentation order: {sorted}; identical to ffprobe: {same}");
            if !same {
                let head: Vec<f64> = values.iter().take(4).map(|(v, s)| *v as f64 / f64::from(*s)).collect();
                let tail: Vec<f64> = values.iter().rev().take(2).map(|(v, s)| *v as f64 / f64::from(*s)).collect();
                println!("decoder pts head {head:?} tail {tail:?}; ffprobe head {:?} tail {:?}", probed.pts.iter().take(4).map(|p| probed.seconds(*p)).collect::<Vec<_>>(), probed.pts.iter().rev().take(2).map(|p| probed.seconds(*p)).collect::<Vec<_>>());
            }
        }
    }
    Ok(())
}

struct Locked<'a>(&'a IOSurfaceRef);

impl<'a> Locked<'a> {
    fn new(surface: &'a IOSurfaceRef) -> Self {
        unsafe { surface.lock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut()) };
        Locked(surface)
    }
}

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        unsafe { self.0.unlock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut()) };
    }
}

fn luma_id(surface: &IOSurfaceRef, width: usize) -> u64 {
    let _lock = Locked::new(surface);
    let base = surface.base_address_of_plane(0).as_ptr() as *const u8;
    let stride = surface.bytes_per_row_of_plane(0);
    let cell = width / 48;
    let mut id = 0u64;
    for bit in 0..verify::BITS {
        let luma = unsafe { *base.add((cell / 2) * stride + bit * cell + cell / 2) };
        if luma > 125 {
            id |= 1 << bit;
        }
    }
    id
}

fn read_bgra(surface: &IOSurfaceRef, layout: &Layout) -> Reading {
    let _lock = Locked::new(surface);
    let base = surface.base_address().as_ptr() as *const u8;
    let stride = surface.bytes_per_row();
    verify::read(layout, |x, y| unsafe {
        let p = base.add(y * stride + x * 4);
        [*p.add(2), *p.add(1), *p]
    })
}

fn read_packed(base: *const u8, width: usize, rgba: bool, layout: &Layout) -> Reading {
    verify::read(layout, |x, y| unsafe {
        let p = base.add((y * width + x) * 4);
        if rgba { [*p, *p.add(1), *p.add(2)] } else { [*p.add(2), *p.add(1), *p] }
    })
}

#[derive(Default)]
struct Row {
    prep: f64,
    seek: f64,
    paint: f64,
    post: f64,
    total: f64,
    cpu: f64,
    id: i64,
    expected_id: i64,
    chosen: usize,
    colour_error: f64,
}

fn cpu_seconds() -> f64 {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let seconds = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 * 1e-6;
    seconds(usage.ru_utime) + seconds(usage.ru_stime)
}

fn children_cpu() -> f64 {
    let output = std::process::Command::new("ps").args(["-ax", "-o", "ppid=,time="]).output().unwrap();
    let me = std::process::id().to_string();
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let (parent, time) = (fields.next()?, fields.next()?);
            if parent != me {
                return None;
            }
            let (minutes, seconds) = time.rsplit_once(':')?;
            let (hours, minutes) = minutes.rsplit_once(':').unwrap_or(("0", minutes));
            Some(hours.parse::<f64>().ok()? * 3600.0 + minutes.parse::<f64>().ok()? * 60.0 + seconds.parse::<f64>().ok()?)
        })
        .sum()
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn summary(label: &str, values: &[f64]) -> String {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let at = |p: f64| sorted[((sorted.len() - 1) as f64 * p) as usize];
    let mean = sorted.iter().sum::<f64>() / sorted.len() as f64;
    format!("  {label:<14} mean {mean:8.3}  p50 {:8.3}  p99 {:8.3}  max {:8.3}", at(0.5), at(0.99), sorted[sorted.len() - 1])
}

fn colour_space(name: &str) -> serde_json::Value {
    match name {
        "none" => serde_json::Value::Null,
        "bt709" => serde_json::json!({"primaries": "bt709", "transfer": "bt709", "matrix": "bt709", "fullRange": false}),
        "srgb" => serde_json::json!({"primaries": "bt709", "transfer": "iec61966-2-1", "matrix": "bt709", "fullRange": false}),
        "bt601" => serde_json::json!({"primaries": "smpte170m", "transfer": "smpte170m", "matrix": "smpte170m", "fullRange": false}),
        "srgb601" => serde_json::json!({"primaries": "bt709", "transfer": "iec61966-2-1", "matrix": "smpte170m", "fullRange": false}),
        "srgbfull601" => serde_json::json!({"primaries": "bt709", "transfer": "iec61966-2-1", "matrix": "smpte170m", "fullRange": true}),
        other => panic!("unknown colour space {other}"),
    }
}

fn run(ui: &host::Ui, options: &Options) -> Result<()> {
    let mode = options.get("arg0", "");
    let clip = options.get("arg1", "");
    let matrix_name = options.get("matrix", "bt709");
    let range = options.get("range", "limited");
    let format = options.get("format", "nv12");
    let draw = options.get("draw", "frame");
    let colour = options.get("cs", "srgb");
    let tag = options.get("tag", &format!("{mode}-{}", std::path::Path::new(&clip).file_stem().unwrap().to_string_lossy()));
    let matrix = if matrix_name == "bt601" { gpu::Matrix::Bt601 } else { gpu::Matrix::Bt709 };
    let ffmpeg_matrix = if matrix_name == "bt601" { "bt601" } else { "bt709" };

    let probed = verify::probe(&clip)?;
    let layout = Layout { width: probed.width, height: probed.height };
    let (w, h) = (probed.width, probed.height);
    let total_frames = probed.output_frames(FPS).min(options.number("frames", usize::MAX));
    let expected = verify::expected(&clip, ffmpeg_matrix, &range, &layout)?;
    ensure!(expected.len() == probed.pts.len(), "ffmpeg produced {} frames, ffprobe {}", expected.len(), probed.pts.len());
    for (index, reading) in expected.iter().enumerate() {
        ensure!(reading.id == index as u64, "source frame {index} has burned-in id {}", reading.id);
    }

    let uses_decoder = mode != "video";
    let pixel_format = if range == "full" { decoder::NV12_FULL_RANGE } else { decoder::NV12_VIDEO_RANGE };
    let mut decoded = if uses_decoder { Some(decoder::decode(&clip, pixel_format)?) } else { None };
    if let Some(decoded) = decoded.as_mut() {
        let first = probed.seconds(probed.pts[0]);
        let before = decoded.frames.len();
        decoded.frames.retain(|f| f.seconds() >= first - 1e-6);
        if before != decoded.frames.len() {
            println!("dropped {} decoder frame(s) before the first container pts {first}", before - decoded.frames.len());
        }
    }
    if let Some(decoded) = &decoded {
        println!("decode: {} frames in {:.3} s = {:.1} fps", decoded.frames.len(), decoded.seconds, decoded.frames.len() as f64 / decoded.seconds);
        ensure!(decoded.frames.len() == probed.pts.len(), "decoder returned {} frames, ffprobe {}", decoded.frames.len(), probed.pts.len());
    }
    let ticks_per_second = probed.tick_den as f64 / probed.tick_num as f64;
    let decoder_pts: Vec<i64> = decoded
        .as_ref()
        .map(|d| d.frames.iter().map(|f| (f.seconds() * ticks_per_second).round() as i64).collect())
        .unwrap_or_default();
    if let Some(decoded) = &decoded {
        ensure!(decoded.frames.windows(2).all(|pair| pair[0].value <= pair[1].value), "decoder frames are not in presentation order");
    }

    let transparent = mode == "composite";
    let (page_name, kind, init_extra) = match mode.as_str() {
        "fetch" | "shared" if draw == "webgl" => ("lab.html", "webgl", serde_json::json!({})),
        "fetch" | "shared" => ("lab.html", "canvas", serde_json::json!({})),
        "image-bmp" | "image-jpeg" => ("lab.html", "img", serde_json::json!({})),
        "composite" => ("lab.html", "overlay", serde_json::json!({})),
        "video" => ("lab.html", "video", serde_json::json!({"src": format!("file://{clip}")})),
        other => bail!("unknown mode {other}"),
    };
    let mut view = View::open(ui, &page(page_name), w as i32, h as i32, transparent)?;
    let mut init = serde_json::json!({"kind": kind, "w": w, "h": h});
    init.as_object_mut().unwrap().extend(init_extra.as_object().unwrap().clone());
    let init_result = view.evaluate(&format!("init({init})"))?;
    println!("page init: {init_result}");

    let needs_gpu = matches!(mode.as_str(), "composite" | "image-bmp" | "image-jpeg") || (matches!(mode.as_str(), "fetch" | "shared") && format != "nv12");
    let mut converter = if needs_gpu {
        let mut converter = gpu::Converter::new(vrf::gpu::Gpu::new()?, w as u32, h as u32);
        if let Some(decoded) = &decoded {
            for (index, frame) in decoded.frames.iter().enumerate() {
                converter.video_views(index, &frame.surface())?;
            }
        }
        Some(converter)
    } else {
        None
    };
    let (kr, kb) = matrix.coefficients();
    let params = |layout_flag: u32| gpu::Params {
        width: w as u32,
        height: h as u32,
        kr,
        kb,
        limited: u32::from(range == "limited"),
        layout: layout_flag,
        pad: [0; 2],
    };

    let mut rows: Vec<Row> = Vec::new();
    let mut failures = 0usize;
    let cs = colour_space(&colour);
    let mut worst_colour: f64 = 0.0;
    let mut patch_worst = [[0.0f64; 3]; verify::PATCHES];
    let mut chosen_log = Vec::new();

    let mut children_start = 0.0;
    let mut children_start_frame = 0;
    for i in 0..total_frames {
        if i == WARMUP {
            children_start = children_cpu();
            children_start_frame = i;
        }
        let target_probe = verify::target_ticks(i, FPS, probed.tick_den / probed.tick_num);
        let want = verify::choose(&probed.pts, target_probe);
        let chosen = if uses_decoder {
            verify::choose(&decoder_pts, target_probe)
        } else {
            want
        };
        let name = format!("f{i}");
        let cpu_start = cpu_seconds();
        let started = Instant::now();
        let mut prepared: Option<(usize, Arc<Blob>)> = None;
        let mut shared: Option<(usize, Box<dyn FnOnce(*mut u8) + Send>)> = None;
        let mut request = serde_json::json!({"i": i});
        let mut mapped = false;

        match mode.as_str() {
            "fetch" | "shared" => {
                let frame = &decoded.as_ref().unwrap().frames[chosen];
                let surface = frame.surface();
                let timestamp = (i as i64) * 1_000_000 / FPS;
                request["src"] = mode.clone().into();
                request["draw"] = draw.clone().into();
                request["ts"] = timestamp.into();
                request["url"] = host::url(&name).into();
                let (parts, size, keep): (Vec<(*const u8, usize)>, usize, Box<dyn std::any::Any + Send + Sync>) = if format == "nv12" {
                    let lock = Locked::new(&surface);
                    std::mem::forget(lock);
                    let stride0 = surface.bytes_per_row_of_plane(0);
                    let stride1 = surface.bytes_per_row_of_plane(1);
                    let p0 = surface.base_address_of_plane(0).as_ptr() as *const u8;
                    let p1 = surface.base_address_of_plane(1).as_ptr() as *const u8;
                    request["fmt"] = "NV12".into();
                    request["layout"] = serde_json::json!([{"offset": 0, "stride": stride0}, {"offset": stride0 * h, "stride": stride1}]);
                    request["cs"] = cs.clone();
                    request["kr"] = kr.into();
                    request["kb"] = kb.into();
                    request["limited"] = u32::from(range == "limited").into();
                    (vec![(p0, stride0 * h), (p1, stride1 * (h / 2))], stride0 * h + stride1 * (h / 2), Box::new(SendSurface(surface.clone())))
                } else {
                    let conv = converter.as_mut().unwrap();
                    let rgba = format == "rgba";
                    conv.run(gpu::Kind::Bgra, chosen, None, params(u32::from(rgba)))?;
                    let ptr = conv.map(conv.size)?;
                    mapped = true;
                    if draw == "frame" || draw == "bitmap" {
                        request["fmt"] = (if rgba { "RGBA" } else { "BGRA" }).into();
                        request["cs"] = cs.clone();
                    }
                    (vec![(ptr, w * h * 4)], w * h * 4, Box::new(()))
                };
                if mode == "fetch" {
                    let blob = unsafe { Blob::new(parts.clone(), keep) };
                    host::publish(&name, blob.clone());
                    prepared = Some((size, blob));
                } else {
                    let parts: Vec<(usize, usize)> = parts.into_iter().map(|(p, l)| (p as usize, l)).collect();
                    shared = Some((size, Box::new(move |dst: *mut u8| {
                        let mut offset = 0;
                        for (ptr, len) in parts {
                            unsafe { std::ptr::copy_nonoverlapping(ptr as *const u8, dst.add(offset), len) };
                            offset += len;
                        }
                        drop(keep);
                    })));
                }
            }
            "image-bmp" => {
                let conv = converter.as_mut().unwrap();
                conv.run(gpu::Kind::Bmp, chosen, None, params(0))?;
                let ptr = conv.map((w * h * 3) as u64)?;
                mapped = true;
                let mut header = vec![0u8; 54];
                let pixels = w * h * 3;
                header[0..2].copy_from_slice(b"BM");
                header[2..6].copy_from_slice(&((54 + pixels) as u32).to_le_bytes());
                header[10..14].copy_from_slice(&54u32.to_le_bytes());
                header[14..18].copy_from_slice(&40u32.to_le_bytes());
                header[18..22].copy_from_slice(&(w as u32).to_le_bytes());
                header[22..26].copy_from_slice(&(h as u32).to_le_bytes());
                header[26..28].copy_from_slice(&1u16.to_le_bytes());
                header[28..30].copy_from_slice(&24u16.to_le_bytes());
                header[34..38].copy_from_slice(&(pixels as u32).to_le_bytes());
                let header = header.into_boxed_slice();
                let parts = vec![(header.as_ptr(), 54), (ptr, pixels)];
                let blob = unsafe { Blob::new(parts, Box::new(header)) };
                host::publish(&name, blob.clone());
                prepared = Some((54 + pixels, blob));
                request["src"] = "img".into();
                request["url"] = host::url(&name).into();
            }
            "image-jpeg" => {
                let conv = converter.as_mut().unwrap();
                conv.run(gpu::Kind::Bgra, chosen, None, params(0))?;
                let ptr = conv.map(conv.size)?;
                mapped = true;
                let quality: f64 = options.get("quality", "0.95").parse()?;
                let mut out = vec![0u8; w * h * 4];
                let length = unsafe { vsp_jpeg(ptr, w, h, w * 4, quality, out.as_mut_ptr(), out.len()) };
                ensure!(length > 0 && length <= out.len(), "jpeg encode failed");
                out.truncate(length);
                let out = out.into_boxed_slice();
                let parts = vec![(out.as_ptr(), length)];
                let blob = unsafe { Blob::new(parts, Box::new(out)) };
                host::publish(&name, blob.clone());
                prepared = Some((length, blob));
                request["src"] = "img".into();
                request["url"] = host::url(&name).into();
            }
            "video" => {
                request["src"] = "video".into();
                let offset: f64 = options.get("offset", "0").parse()?;
                let epsilon: f64 = options.get("epsilon", "0").parse()?;
                request["t"] = (i as f64 / FPS as f64 - offset + epsilon).into();
            }
            "composite" => {
                request["src"] = "none".into();
            }
            _ => unreachable!(),
        }
        if mapped && prepared.is_none() && shared.is_none() {
            bail!("mapped buffer without a consumer");
        }
        let prep = started.elapsed();

        let seek_started = Instant::now();
        view.drain_paints();
        if let Some((size, fill)) = shared.take() {
            view.send_shared(size, fill)?;
        }
        let expression = if mode == "composite" { format!("tick({i})") } else { format!("show({request})") };
        let result = view.evaluate(&expression).with_context(|| format!("frame {i}"))?;
        if mode == "video" && i < 3 {
            println!("video frame {i}: currentTime {result}");
        }
        let seek = seek_started.elapsed();

        let sent = view.begin_frame();
        let paint = view.recv_paint(PAINT_TIMEOUT).with_context(|| format!("no paint for frame {i}"))?;
        ensure!(paint.at >= sent, "stale paint at frame {i}");
        let paint_time = paint.at - sent;

        let post_started = Instant::now();
        let mut composite_ptr: Option<*const u8> = None;
        if mode == "composite" {
            let conv = converter.as_mut().unwrap();
            conv.page_view(&paint.surface)?;
            conv.run(gpu::Kind::Composite, chosen, Some(paint.surface.id()), params(0))?;
        }
        let post = post_started.elapsed();
        let total = started.elapsed();
        let cpu = cpu_seconds() - cpu_start;

        if mode == "composite" {
            let conv = converter.as_mut().unwrap();
            composite_ptr = Some(conv.map(conv.size)?);
            mapped = true;
        }
        let reading = match composite_ptr {
            Some(ptr) => read_packed(ptr, w, false, &layout),
            None => read_bgra(&paint.surface, &layout),
        };
        let mut reference = expected[want].clone();
        if mode == "composite" {
            for c in 0..3 {
                reference.patches[1][c] *= 0.5;
            }
            reference.patches[2] = [255.0, 0.0, 255.0];
        }
        let colour_error = verify::max_patch_error(&reading, &reference);
        let ok = reading.id == reference.id && chosen == want;
        if !ok {
            failures += 1;
        }
        worst_colour = worst_colour.max(colour_error);
        for p in 0..verify::PATCHES {
            for c in 0..3 {
                patch_worst[p][c] = patch_worst[p][c].max((reading.patches[p][c] - reference.patches[p][c]).abs());
            }
        }
        if i == 4 {
            for p in 0..verify::PATCHES {
                println!("patch {p}: got {:?} want {:?}", reading.patches[p].map(|v| v.round()), reference.patches[p].map(|v| v.round()));
            }
        }
        chosen_log.push((i, want, chosen, reading.id));

        if mapped {
            converter.as_ref().unwrap().unmap();
        }
        if let Some((_, blob)) = prepared.take() {
            drop(blob);
            host::retract(&name);
        }
        if format == "nv12" && matches!(mode.as_str(), "fetch" | "shared") {
            let frame = &decoded.as_ref().unwrap().frames[chosen];
            unsafe { frame.surface().unlock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut()) };
        }

        rows.push(Row {
            prep: ms(prep),
            seek: ms(seek),
            paint: ms(paint_time),
            post: ms(post),
            total: ms(total),
            cpu: cpu * 1000.0,
            id: reading.id as i64,
            expected_id: reference.id as i64,
            chosen,
            colour_error,
        });
    }
    let children = (children_cpu() - children_start) * 1000.0 / (total_frames - children_start_frame).max(1) as f64;
    view.close()?;

    let measured = &rows[WARMUP.min(rows.len() - 1)..];
    let column = |f: fn(&Row) -> f64| measured.iter().map(f).collect::<Vec<_>>();
    println!("uptime: {}", uptime());
    println!("mode {mode} clip {clip} format {format} draw {draw} cs {colour} range {range} matrix {matrix_name}");
    println!("frames {} (timed {}), id mismatches {failures}, worst patch error {worst_colour:.2}", rows.len(), measured.len());
    println!("worst per-patch error over all frames (r,g,b): {:?}", patch_worst.map(|p| p.map(|v| (v * 10.0).round() / 10.0)));
    println!("per-frame ms (prep = host work before the page call, seek = page call incl. DevTools round trip, paint = begin frame to paint, post = GPU work after paint):");
    for (label, f) in [
        ("prep", (|r: &Row| r.prep) as fn(&Row) -> f64),
        ("seek", |r| r.seek),
        ("paint", |r| r.paint),
        ("post", |r| r.post),
        ("total", |r| r.total),
        ("host cpu", |r| r.cpu),
    ] {
        println!("{}", summary(label, &column(f)));
    }
    println!("  child cpu      {children:8.3} ms/frame (renderer, GPU and utility processes, ps cputime, coarse)");
    let dir = format!("{}/../../docs/spikes/raw/5.1", env!("CARGO_MANIFEST_DIR"));
    let mut csv = String::from("frame,chosen,expected_chosen,read_id,expected_id,prep_ms,seek_ms,paint_ms,post_ms,total_ms,host_cpu_ms,patch_error\n");
    for (index, row) in rows.iter().enumerate() {
        let (_, want, _, _) = chosen_log[index];
        csv.push_str(&format!(
            "{index},{},{want},{},{},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.2}\n",
            row.chosen, row.id, row.expected_id, row.prep, row.seek, row.paint, row.post, row.total, row.cpu, row.colour_error
        ));
    }
    std::fs::write(format!("{dir}/{tag}.csv"), csv)?;
    Ok(())
}

struct SendSurface(objc2_core_foundation::CFRetained<IOSurfaceRef>);
unsafe impl Send for SendSurface {}
unsafe impl Sync for SendSurface {}
