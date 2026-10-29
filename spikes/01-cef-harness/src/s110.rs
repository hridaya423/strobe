use crate::{convert::Nv12Converter, gpu::Gpu, harness::Harness, page_url};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFMutableDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::{CMSampleBuffer, CMTime, CMVideoFormatDescriptionGetH264ParameterSetAtIndex, kCMTimeInvalid, kCMVideoCodecType_H264};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferPool, kCVImageBufferColorPrimaries_ITU_R_709_2, kCVImageBufferTransferFunction_ITU_R_709_2,
    kCVImageBufferYCbCrMatrix_ITU_R_709_2, kCVPixelBufferHeightKey, kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferPoolMinimumBufferCountKey, kCVPixelBufferWidthKey,
    kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
};
use objc2_metal::MTLTextureUsage;
use objc2_video_toolbox::{
    VTCompressionSession, VTEncodeInfoFlags, VTSession, kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AverageBitRate,
    kVTCompressionPropertyKey_ColorPrimaries, kVTCompressionPropertyKey_ExpectedFrameRate, kVTCompressionPropertyKey_MaxKeyFrameInterval,
    kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality, kVTCompressionPropertyKey_RealTime, kVTCompressionPropertyKey_TransferFunction,
    kVTCompressionPropertyKey_YCbCrMatrix, kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
};
use std::{
    cell::RefCell,
    collections::HashMap,
    ffi::{c_char, c_void},
    path::PathBuf,
    process::Command,
    ptr::{NonNull, null_mut},
    rc::Rc,
    sync::Mutex,
    time::{Duration, Instant},
};

unsafe extern "C" {
    fn CVPixelBufferGetIOSurface(pixel_buffer: *const c_void) -> *mut c_void;
    fn IOSurfaceGetID(surface: *const c_void) -> u32;
}

#[derive(Default)]
struct Sink {
    annexb: Vec<u8>,
    frames: usize,
    errors: usize,
}

const START_CODE: [u8; 4] = [0, 0, 0, 1];

unsafe extern "C-unwind" fn on_encoded(refcon: *mut c_void, _frame: *mut c_void, status: i32, _flags: VTEncodeInfoFlags, sample: *mut CMSampleBuffer) {
    let sink = unsafe { &*(refcon as *const Mutex<Sink>) };
    let mut sink = sink.lock().unwrap();
    if status != 0 || sample.is_null() {
        sink.errors += 1;
        return;
    }
    let sample = unsafe { &*sample };
    let block = unsafe { sample.data_buffer() }.expect("encoded sample without data");
    let (mut length, mut total, mut pointer): (usize, usize, *mut c_char) = (0, 0, null_mut());
    assert_eq!(unsafe { block.data_pointer(0, &mut length, &mut total, &mut pointer) }, 0);
    assert_eq!(length, total, "block buffer is not contiguous");
    let data = unsafe { std::slice::from_raw_parts(pointer as *const u8, total) };
    let first_nal_type = data.get(4).map(|b| b & 0x1f);
    if first_nal_type == Some(5) || sink.frames == 0 {
        let format = unsafe { sample.format_description() }.expect("no format description");
        let mut index = 0;
        loop {
            let (mut set, mut size, mut count, mut header): (*const u8, usize, usize, i32) = (std::ptr::null(), 0, 0, 0);
            let status = unsafe { CMVideoFormatDescriptionGetH264ParameterSetAtIndex(&format, index, &mut set, &mut size, &mut count, &mut header) };
            assert_eq!(status, 0);
            sink.annexb.extend_from_slice(&START_CODE);
            sink.annexb.extend_from_slice(unsafe { std::slice::from_raw_parts(set, size) });
            index += 1;
            if index >= count {
                break;
            }
        }
    }
    let mut offset = 0;
    while offset + 4 <= data.len() {
        let size = u32::from_be_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        sink.annexb.extend_from_slice(&START_CODE);
        sink.annexb.extend_from_slice(&data[offset + 4..offset + 4 + size]);
        offset += 4 + size;
    }
    sink.frames += 1;
}

fn property(session: &VTSession, key: &CFString, value: &CFType) {
    let status = unsafe { objc2_video_toolbox::VTSessionSetProperty(session, key, Some(value)) };
    if status != 0 {
        println!("  VTSessionSetProperty {key:?} -> {status}");
    }
}

struct Timing {
    import: f64,
    convert_wait: f64,
    submit: f64,
    total: f64,
}

fn stat(label: &str, mut v: Vec<f64>) {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("  {label}: p50={:.3} p95={:.3} max={:.3}", v[v.len() / 2], v[(v.len() - 1) * 95 / 100], v.last().unwrap());
}

pub fn run(options: &HashMap<String, String>) {
    let n: usize = options.get("n").map_or(300, |v| v.parse().unwrap());
    let size = options.get("size").map_or("1080", String::as_str);
    let (width, height) = if size == "4k" { (3840u32, 2160u32) } else { (1920, 1080) };
    let out = PathBuf::from(options.get("out").expect("out=<dir>"));
    let verify = options.get("verify").is_some_and(|v| v == "1");
    std::fs::create_dir_all(&out).unwrap();

    let h = Harness::open(&page_url("s13.html"), width as i32, height as i32);
    let converter = Rc::new(Nv12Converter::new(&h.shared.gpu));

    let pool_attributes = CFMutableDictionary::<CFString, CFType>::empty();
    pool_attributes.add(unsafe { kCVPixelBufferPoolMinimumBufferCountKey }, &CFNumber::new_i32(8));
    let buffer_attributes = CFMutableDictionary::<CFString, CFType>::empty();
    unsafe {
        buffer_attributes.add(kCVPixelBufferPixelFormatTypeKey, &CFNumber::new_i32(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange as i32));
        buffer_attributes.add(kCVPixelBufferWidthKey, &CFNumber::new_i32(width as i32));
        buffer_attributes.add(kCVPixelBufferHeightKey, &CFNumber::new_i32(height as i32));
        buffer_attributes.add(kCVPixelBufferIOSurfacePropertiesKey, &*CFDictionary::<CFString, CFType>::empty());
        buffer_attributes.add(kCVPixelBufferMetalCompatibilityKey, CFBoolean::new(true));
    }
    let mut pool_ptr: *mut CVPixelBufferPool = null_mut();
    let status = unsafe {
        CVPixelBufferPool::create(None, Some(pool_attributes.as_opaque()), Some(buffer_attributes.as_opaque()), NonNull::new(&mut pool_ptr).unwrap())
    };
    assert_eq!(status, 0, "CVPixelBufferPoolCreate");
    let pool = unsafe { CFRetained::from_raw(NonNull::new(pool_ptr).unwrap()) };

    let sink: &'static Mutex<Sink> = Box::leak(Box::default());
    let specification = CFMutableDictionary::<CFString, CFType>::empty();
    specification.add(unsafe { kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder }, CFBoolean::new(true));
    let mut session_ptr: *mut VTCompressionSession = null_mut();
    let status = unsafe {
        VTCompressionSession::create(
            None,
            width as i32,
            height as i32,
            kCMVideoCodecType_H264,
            Some(specification.as_opaque()),
            None,
            None,
            Some(on_encoded),
            sink as *const Mutex<Sink> as *mut c_void,
            NonNull::new(&mut session_ptr).unwrap(),
        )
    };
    assert_eq!(status, 0, "VTCompressionSessionCreate");
    let session: CFRetained<VTCompressionSession> = unsafe { CFRetained::from_raw(NonNull::new(session_ptr).unwrap()) };
    let session_ref: &VTSession = session.as_ref();
    unsafe {
        property(session_ref, kVTCompressionPropertyKey_RealTime, CFBoolean::new(false));
        property(session_ref, kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality, CFBoolean::new(true));
        property(session_ref, kVTCompressionPropertyKey_AllowFrameReordering, CFBoolean::new(false));
        property(session_ref, kVTCompressionPropertyKey_AverageBitRate, &CFNumber::new_i32(if size == "4k" { 40_000_000 } else { 16_000_000 }));
        property(session_ref, kVTCompressionPropertyKey_ExpectedFrameRate, &CFNumber::new_i32(30));
        property(session_ref, kVTCompressionPropertyKey_MaxKeyFrameInterval, &CFNumber::new_i32(120));
        property(session_ref, kVTCompressionPropertyKey_ColorPrimaries, kCVImageBufferColorPrimaries_ITU_R_709_2);
        property(session_ref, kVTCompressionPropertyKey_TransferFunction, kCVImageBufferTransferFunction_ITU_R_709_2);
        property(session_ref, kVTCompressionPropertyKey_YCbCrMatrix, kCVImageBufferYCbCrMatrix_ITU_R_709_2);
        session.prepare_to_encode_frames();
    }

    let session = Rc::new(session);
    let timings: Rc<RefCell<Vec<Timing>>> = Rc::default();
    let planes: Rc<RefCell<HashMap<u32, (wgpu::Texture, wgpu::Texture)>>> = Rc::default();
    let next_pts = Rc::new(std::cell::Cell::new(0i64));
    let armed = Rc::new(std::cell::Cell::new(false));
    let reference_frames: Vec<usize> = vec![0, n / 2, n - 1];
    {
        let (shared, converter, session, timings, planes, next_pts, armed) =
            (h.shared.clone(), converter.clone(), session.clone(), timings.clone(), planes.clone(), next_pts.clone(), armed.clone());
        let (out, reference_frames) = (out.clone(), reference_frames.clone());
        h.set_paint(move |info| {
            if !armed.get() {
                return;
            }
            let t0 = Instant::now();
            let gpu: &Gpu = &shared.gpu;
            let frame = gpu.import_frame(info);
            let t1 = Instant::now();
            let mut buffer_ptr: *mut CVPixelBuffer = null_mut();
            let status = unsafe { CVPixelBufferPool::create_pixel_buffer(None, &pool, NonNull::new(&mut buffer_ptr).unwrap()) };
            assert_eq!(status, 0, "CVPixelBufferPoolCreatePixelBuffer");
            let buffer: CFRetained<CVPixelBuffer> = unsafe { CFRetained::from_raw(NonNull::new(buffer_ptr).unwrap()) };
            let surface = unsafe { CVPixelBufferGetIOSurface(buffer_ptr as *const c_void) };
            let id = unsafe { IOSurfaceGetID(surface) };
            let mut planes = planes.borrow_mut();
            let (y, c) = planes.entry(id).or_insert_with(|| unsafe {
                let usage = wgpu::TextureUsages::RENDER_ATTACHMENT;
                let mtl = MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead;
                (
                    gpu.import_plane(surface, 0, width, height, wgpu::TextureFormat::R8Unorm, usage, mtl, wgpu::TextureUses::COLOR_TARGET),
                    gpu.import_plane(surface, 1, width / 2, height / 2, wgpu::TextureFormat::Rg8Unorm, usage, mtl, wgpu::TextureUses::COLOR_TARGET),
                )
            });
            let origin = (info.extra.visible_rect.x as u32, info.extra.visible_rect.y as u32);
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            converter.encode(gpu, &mut encoder, &frame.texture, origin, y, c);
            gpu.queue.submit([encoder.finish()]);
            gpu.wait();
            let t2 = Instant::now();
            let index = next_pts.get();
            next_pts.set(index + 1);
            let status = unsafe {
                session.encode_frame(&buffer, CMTime::new(index, 30), CMTime::new(1, 30), None, null_mut(), null_mut())
            };
            assert_eq!(status, 0, "VTCompressionSessionEncodeFrame");
            let t3 = Instant::now();
            let ms = |a: Instant, b: Instant| (b - a).as_secs_f64() * 1000.0;
            timings.borrow_mut().push(Timing { import: ms(t0, t1), convert_wait: ms(t1, t2), submit: ms(t2, t3), total: ms(t0, t3) });
            if verify && reference_frames.contains(&(index as usize)) {
                let bytes = gpu.read_frame(&frame, origin, (width, height));
                crate::gpu::save_png(&out.join(format!("ref-{index}.png")), &bytes, frame.swap_rb, width, height);
            }
        });
    }

    for i in 0..5 {
        h.eval(&format!("seek({i})"));
        h.begin_frame();
        h.pump_for(Duration::from_millis(100));
    }
    armed.set(true);

    let started = Instant::now();
    let mut dropped = 0;
    for i in 0..n {
        h.eval(&format!("seek({})", i + 100));
        let before = timings.borrow().len();
        h.begin_frame();
        if !h.pump_until(Duration::from_millis(500), || timings.borrow().len() > before) {
            dropped += 1;
        }
    }
    let loop_seconds = started.elapsed().as_secs_f64();
    unsafe { session.complete_frames(kCMTimeInvalid) };
    let sink_guard = sink.lock().unwrap();
    let h264 = out.join("frames.h264");
    std::fs::write(&h264, &sink_guard.annexb).unwrap();
    println!("spike=1.10 size={width}x{height} frames={n} dropped={dropped} encoded_samples={} encoder_errors={} raster={} load={}", sink_guard.frames, sink_guard.errors, crate::harness::raster_mode(), crate::load_average());
    println!("  loop wall {loop_seconds:.2} s = {:.1} frames/s (lock-step, includes seek and Chromium); cpu_pixel_readbacks={}", n as f64 / loop_seconds, crate::gpu::cpu_readbacks());
    let timings = timings.borrow();
    stat("callback total ms (import + convert + GPU wait + VT submit)", timings.iter().map(|t| t.total).collect());
    stat("  import ms", timings.iter().map(|t| t.import).collect());
    stat("  convert into NV12 IOSurface + GPU wait ms", timings.iter().map(|t| t.convert_wait).collect());
    stat("  VTCompressionSessionEncodeFrame ms", timings.iter().map(|t| t.submit).collect());

    let mp4 = out.join("out.mp4");
    let ffmpeg = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "h264", "-framerate", "30", "-i"])
        .arg(&h264)
        .args(["-c", "copy", "-movflags", "+faststart"])
        .arg(&mp4)
        .status()
        .unwrap();
    println!("  ffmpeg remux (stream copy, no re-encode) status={ffmpeg}");
    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-count_frames", "-select_streams", "v:0", "-show_entries", "stream=codec_name,profile,width,height,nb_read_frames,color_space,color_primaries,color_transfer,color_range,pix_fmt", "-of", "default=nw=1"])
        .arg(&mp4)
        .output()
        .unwrap();
    println!("  ffprobe -count_frames:\n{}", String::from_utf8_lossy(&probe.stdout).lines().map(|l| format!("    {l}")).collect::<Vec<_>>().join("\n"));
    for i in &reference_frames {
        let decoded = out.join(format!("dec-{i}.png"));
        Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-i"])
            .arg(&mp4)
            .args(["-vf", &format!("select=eq(n\\,{i})"), "-vframes", "1"])
            .arg(&decoded)
            .status()
            .unwrap();
        if verify {
            let psnr = Command::new("ffmpeg")
                .args(["-hide_banner", "-i"])
                .arg(&decoded)
                .arg("-i")
                .arg(out.join(format!("ref-{i}.png")))
                .args(["-lavfi", "psnr", "-f", "null", "-"])
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&psnr.stderr);
            let line = text.lines().find(|l| l.contains("average:")).unwrap_or("psnr failed");
            println!("  frame {i}: decoded-vs-source {}", line.split("PSNR").nth(1).unwrap_or(line).trim());
        }
    }
    h.close();
}
