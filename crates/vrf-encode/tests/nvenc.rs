#![cfg(windows)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::Result;
use vrf_encode::nvenc::{
    NvencEncoder, NvencSessions, Options, POOL_SIZE, PixelFormat, PoolFrame, Preset, RateControl,
    Ready, Tuning, capabilities, nvidia_device,
};
use vrf_encode::{
    Codec, Encoder, EncoderConfig, EncoderFrame, Packet, Quality, Rational, StreamInfo,
};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_UNKNOWN;
use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
use windows::core::Interface;

#[allow(dead_code)]
mod video_support;

use video_support::*;

static ENGINE: Mutex<()> = Mutex::new(());

fn engine() -> MutexGuard<'static, ()> {
    ENGINE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn config(codec: Codec, frame_rate: Rational) -> EncoderConfig {
    let mut config = EncoderConfig::new(codec, WIDTH as u32, HEIGHT as u32, frame_rate);
    config.keyframe_interval = 30;
    config
}

fn options(b_frames: u32) -> Options {
    Options {
        preset: Preset::P4,
        tuning: Tuning::HighQuality,
        rate_control: RateControl::ConstantQuality(24),
        b_frames,
    }
}

fn encode(
    config: EncoderConfig,
    options: Options,
    frames: usize,
) -> Result<(Vec<Packet>, StreamInfo)> {
    let device = nvidia_device()?;
    let mut encoder = NvencEncoder::new(&device, config.clone(), options)?;
    let mut uploader = Uploader::new(
        &device,
        config.width,
        config.height,
        encoder.pool().format(),
    );
    let mut packets = Vec::new();
    for index in 0..frames {
        let mut frame = encoder.pool().acquire();
        uploader.write(&mut frame, flat(index));
        encoder.submit(EncoderFrame::D3d12(frame), pts_of(index, config.frame_rate))?;
        packets.extend(encoder.drain()?);
    }
    packets.extend(encoder.flush()?);
    let info = encoder.finish()?;
    Ok((packets, info))
}

fn check_packets(packets: &[Packet], info: &StreamInfo, frame_rate: Rational, reorders: bool) {
    assert_eq!(packets.len(), FRAMES);
    assert_eq!((info.width, info.height), (WIDTH as u32, HEIGHT as u32));
    assert_eq!(info.timebase, Rational::new(1, frame_rate.num));
    let mut by_pts: Vec<&Packet> = packets.iter().collect();
    by_pts.sort_by_key(|p| p.pts.num * 1_000_000 / p.pts.den);
    for (index, packet) in by_pts.iter().enumerate() {
        assert!(
            same(packet.pts, pts_of(index, frame_rate)),
            "frame {index}: pts {:?}",
            packet.pts
        );
        assert!(!packet.data.is_empty());
    }
    let keyframes: Vec<usize> = (0..FRAMES).filter(|i| by_pts[*i].keyframe).collect();
    assert_eq!(keyframes, [0, 30]);
    for pair in packets.windows(2) {
        let (a, b) = (pair[0].dts, pair[1].dts);
        assert!(
            a.num * b.den < b.num * a.den,
            "dts must increase: {a:?} then {b:?}"
        );
    }
    for packet in packets {
        let (dts, pts) = (packet.dts, packet.pts);
        assert!(
            dts.num * pts.den <= pts.num * dts.den,
            "dts {dts:?} after pts {pts:?}"
        );
    }
    if reorders {
        assert!(
            packets.windows(2).any(|pair| {
                pair[1].pts.num * pair[0].pts.den < pair[0].pts.num * pair[1].pts.den
            }),
            "no packet left presentation order, so no B-frame was used"
        );
    } else {
        assert!(
            packets.windows(2).all(|pair| {
                pair[0].pts.num * pair[1].pts.den < pair[1].pts.num * pair[0].pts.den
            })
        );
    }
}

fn check_mp4(codec: Codec, ten_bit: bool, b_frames: u32, name: &str, expected: &str) {
    let _engine = engine();
    let frame_rate = Rational::new(30000, 1001);
    let mut config = config(codec, frame_rate);
    config.ten_bit = ten_bit;
    let (packets, info) = encode(config, options(b_frames), FRAMES).unwrap();
    check_packets(
        &packets,
        &info,
        frame_rate,
        b_frames > 0 && codec != Codec::Av1,
    );
    assert_eq!(info.codec, codec);
    assert!(!info.config.is_empty());

    let scratch = Scratch::new(name);
    let file = scratch.path("out.mp4");
    write_mp4(&file, packets, &info, frame_rate);
    assert_eq!(
        probe(&file, "stream=codec_name,width,height,nb_read_frames"),
        format!("{expected},{WIDTH},{HEIGHT},60")
    );
    if ten_bit {
        assert_eq!(probe(&file, "stream=pix_fmt"), "yuv420p10le");
    }
    let indexes = decoded_indexes(&file, ten_bit);
    assert_eq!(indexes, (0..FRAMES as i64).collect::<Vec<_>>());
    assert_eq!(indexes[37], 37);
}

#[test]
fn h264_encodes_60_frames_and_frame_37_decodes_as_37() {
    check_mp4(Codec::H264, false, 0, "nv-h264", "h264");
}

#[test]
fn hevc_encodes_60_frames_and_frame_37_decodes_as_37() {
    check_mp4(Codec::Hevc, false, 0, "nv-hevc", "hevc");
}

#[test]
fn hevc_main10_encodes_p010_frames() {
    check_mp4(Codec::Hevc, true, 0, "nv-hevc10", "hevc");
}

#[test]
fn av1_encodes_60_frames_and_frame_37_decodes_as_37() {
    check_mp4(Codec::Av1, false, 0, "nv-av1", "av1");
}

#[test]
fn av1_encodes_10_bit_p010_frames() {
    check_mp4(Codec::Av1, true, 0, "nv-av1-10", "av1");
}

#[test]
fn h264_with_b_frames_keeps_presentation_order_and_valid_dts() {
    check_mp4(Codec::H264, false, 2, "nv-h264-b", "h264");
}

#[test]
fn hevc_with_b_frames_keeps_presentation_order_and_valid_dts() {
    check_mp4(Codec::Hevc, false, 2, "nv-hevc-b", "hevc");
}

#[test]
fn av1_with_b_frames_emits_temporal_units_in_presentation_order() {
    check_mp4(Codec::Av1, false, 2, "nv-av1-b", "av1");
}

fn check_sessions(count: usize, chunk: u64, repeats: &[usize], name: &str) {
    let _engine = engine();
    let frames = 80;
    let frame_rate = Rational::new(30, 1);
    let config = config(Codec::H264, frame_rate);
    let device = nvidia_device().unwrap();
    let mut encoder = NvencSessions::new(&device, config.clone(), options(0), count, 48).unwrap();
    let pools = encoder.pools();
    let mut uploader = Uploader::new(&device, config.width, config.height, pools[0].format());
    let mut packets = Vec::new();
    let mut previous: Option<PoolFrame> = None;
    let mut expected = Vec::new();
    for index in 0..frames {
        let frame = match (&previous, repeats.contains(&index)) {
            (Some(previous), true) => {
                expected.push(*expected.last().unwrap());
                previous.clone()
            }
            _ => {
                let session = NvencSessions::session_of(index as u64, chunk, count);
                let mut frame = pools[session].acquire();
                uploader.write(&mut frame, flat(index));
                expected.push(index as i64);
                frame
            }
        };
        previous = Some(frame.clone());
        encoder
            .submit(EncoderFrame::D3d12(frame), pts_of(index, frame_rate))
            .unwrap();
        packets.extend(encoder.drain().unwrap());
    }
    packets.extend(encoder.flush().unwrap());
    let info = encoder.finish().unwrap();

    assert_eq!(packets.len(), frames);
    for (index, packet) in packets.iter().enumerate() {
        assert!(same(packet.pts, pts_of(index, frame_rate)), "frame {index}");
        assert!(same(packet.dts, packet.pts), "frame {index} dts");
    }
    assert!(packets[0].keyframe);
    assert!(
        packets.iter().filter(|p| p.keyframe).count() > frames / 30,
        "every chunk starts on a keyframe"
    );
    let scratch = Scratch::new(name);
    let file = scratch.path("out.mp4");
    write_mp4(&file, packets, &info, frame_rate);
    assert_eq!(decoded_indexes(&file, false), expected);
}

#[test]
fn two_sessions_write_one_stream_in_order() {
    check_sessions(2, 10, &[], "nv-two-sessions");
}

#[test]
fn three_sessions_write_one_stream_in_order() {
    check_sessions(3, 7, &[], "nv-three-sessions");
}

#[test]
fn a_repeated_frame_at_a_chunk_boundary_stays_in_its_own_session() {
    check_sessions(2, 10, &[20, 30, 31], "nv-two-sessions-repeat");
}

#[test]
fn sessions_reject_b_frames() {
    let _engine = engine();
    let device = nvidia_device().unwrap();
    let result = NvencSessions::new(
        &device,
        config(Codec::H264, Rational::new(30, 1)),
        options(2),
        2,
        8,
    );
    assert!(result.is_err());
}

fn check_late_drain(codec: Codec) {
    let _engine = engine();
    let watchdog = std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(60));
        eprintln!("deadlock: 300 undrained frames did not finish within 60 s");
        std::process::exit(1);
    });
    let device = nvidia_device().unwrap();
    let mut encoder = NvencEncoder::new(
        &device,
        config(codec, Rational::new(30, 1)),
        Quality::Balanced.into(),
    )
    .unwrap();
    let mut uploader = Uploader::new(&device, WIDTH as u32, HEIGHT as u32, PixelFormat::Nv12);
    for index in 0..300 {
        let mut frame = encoder.pool().acquire();
        uploader.write(&mut frame, flat(index % 70));
        encoder
            .submit(EncoderFrame::D3d12(frame), Rational::new(index as i64, 30))
            .unwrap();
    }
    let packets = encoder.flush().unwrap();
    assert_eq!(packets.len(), 300);
    encoder.finish().unwrap();
    drop(watchdog);
}

#[test]
fn h264_caller_that_drains_only_at_the_end_does_not_deadlock() {
    check_late_drain(Codec::H264);
}

#[test]
fn hevc_caller_that_drains_only_at_the_end_does_not_deadlock() {
    check_late_drain(Codec::Hevc);
}

#[test]
fn av1_caller_that_drains_only_at_the_end_does_not_deadlock() {
    check_late_drain(Codec::Av1);
}

#[test]
fn lookahead_preset_with_b_frames_and_late_drain_does_not_deadlock() {
    let _engine = engine();
    let device = nvidia_device().unwrap();
    let mut options = Options::from(Quality::High);
    options.b_frames = 3;
    let mut encoder =
        NvencEncoder::new(&device, config(Codec::Hevc, Rational::new(30, 1)), options).unwrap();
    let mut uploader = Uploader::new(&device, WIDTH as u32, HEIGHT as u32, PixelFormat::Nv12);
    for index in 0..200 {
        let mut frame = encoder.pool().acquire();
        uploader.write(&mut frame, flat(index % 70));
        encoder
            .submit(EncoderFrame::D3d12(frame), Rational::new(index as i64, 30))
            .unwrap();
    }
    assert_eq!(encoder.flush().unwrap().len(), 200);
}

#[test]
fn pool_holds_48_stable_textures_and_blocks_when_exhausted() {
    let _engine = engine();
    let device = nvidia_device().unwrap();
    let encoder = NvencEncoder::new(
        &device,
        config(Codec::H264, Rational::new(30, 1)),
        Quality::Draft.into(),
    )
    .unwrap();
    let pool = encoder.pool();
    assert_eq!(POOL_SIZE, 48);
    let held: Vec<PoolFrame> = (0..POOL_SIZE).map(|_| pool.acquire()).collect();
    let pointers: std::collections::HashSet<_> = held
        .iter()
        .map(|f| f.resource().as_raw() as usize)
        .collect();
    assert_eq!(pointers.len(), POOL_SIZE);
    assert!(pool.try_acquire().is_none());
    let waiter = {
        let pool = pool.clone();
        std::thread::spawn(move || pool.acquire().resource().as_raw() as usize)
    };
    std::thread::sleep(Duration::from_millis(100));
    assert!(!waiter.is_finished());
    drop(held);
    assert!(pointers.contains(&waiter.join().unwrap()));
}

#[test]
fn ten_bit_is_rejected_for_h264() {
    let device = nvidia_device().unwrap();
    let mut config = config(Codec::H264, Rational::new(30, 1));
    config.ten_bit = true;
    let error = NvencEncoder::new(&device, config, Quality::Balanced.into())
        .err()
        .unwrap();
    assert!(error.to_string().contains("ten_bit"), "{error}");
}

#[test]
fn a_frame_from_another_encoders_pool_is_rejected() {
    let _engine = engine();
    let device = nvidia_device().unwrap();
    let make = || {
        NvencEncoder::new(
            &device,
            config(Codec::H264, Rational::new(30, 1)),
            Quality::Draft.into(),
        )
        .unwrap()
    };
    let (mut first, second) = (make(), make());
    let frame = second.pool().acquire();
    let error = first
        .submit(EncoderFrame::D3d12(frame), Rational::new(0, 30))
        .err()
        .unwrap();
    assert!(error.to_string().contains("another encoder"), "{error}");
}

#[test]
fn pts_must_increase() {
    let _engine = engine();
    let device = nvidia_device().unwrap();
    let mut encoder = NvencEncoder::new(
        &device,
        config(Codec::H264, Rational::new(30, 1)),
        Quality::Draft.into(),
    )
    .unwrap();
    let frame = encoder.pool().acquire();
    encoder
        .submit(EncoderFrame::D3d12(frame), Rational::new(1, 30))
        .unwrap();
    let frame = encoder.pool().acquire();
    let error = encoder
        .submit(EncoderFrame::D3d12(frame), Rational::new(1, 30))
        .err()
        .unwrap();
    assert!(error.to_string().contains("pts"), "{error}");
}

#[test]
fn capabilities_report_the_codecs_the_gpu_opens() {
    let device = nvidia_device().unwrap();
    let caps = capabilities(&device).unwrap();
    println!("{caps:#?}");
    assert!(caps.api_version.0 >= 12);
    let codecs: Vec<Codec> = caps.codecs.iter().map(|c| c.codec).collect();
    assert!(codecs.contains(&Codec::H264) && codecs.contains(&Codec::Hevc));
    assert!(caps.codecs.iter().all(|c| c.engines >= 1));
}

#[test]
fn quality_levels_order_the_bitrate() {
    let _engine = engine();
    let device = nvidia_device().unwrap();
    let mut sizes = Vec::new();
    for quality in [Quality::Draft, Quality::Balanced, Quality::High] {
        let mut encoder = NvencEncoder::new(
            &device,
            config(Codec::H264, Rational::new(30, 1)),
            quality.into(),
        )
        .unwrap();
        let mut uploader = Uploader::new(&device, WIDTH as u32, HEIGHT as u32, PixelFormat::Nv12);
        let mut bytes = 0;
        for index in 0..FRAMES {
            let mut frame = encoder.pool().acquire();
            uploader.write(&mut frame, moving(index as u32, WIDTH, HEIGHT));
            encoder
                .submit(EncoderFrame::D3d12(frame), Rational::new(index as i64, 30))
                .unwrap();
        }
        for packet in encoder.flush().unwrap() {
            bytes += packet.data.len();
        }
        println!("{quality:?}: {bytes} bytes");
        sizes.push(bytes);
    }
    assert!(sizes[0] < sizes[1] && sizes[1] < sizes[2], "{sizes:?}");
}

fn session(
    device: &ID3D12Device,
    codec: Codec,
    preset: Preset,
    tuning: Tuning,
    frames: usize,
) -> (f64, usize) {
    let (width, height) = (1920usize, 1080usize);
    let mut config = EncoderConfig::new(codec, width as u32, height as u32, Rational::new(60, 1));
    config.keyframe_interval = 250;
    let options = Options {
        preset,
        tuning,
        rate_control: match tuning {
            Tuning::HighQuality => RateControl::Vbr(20_000_000),
            _ => RateControl::Cbr(20_000_000),
        },
        b_frames: 0,
    };
    let mut encoder = NvencEncoder::new(device, config, options).unwrap();
    let mut uploader = Uploader::new(device, width as u32, height as u32, PixelFormat::Nv12);
    let mut warm: Vec<PoolFrame> = (0..POOL_SIZE).map(|_| encoder.pool().acquire()).collect();
    for (k, frame) in warm.iter_mut().enumerate() {
        uploader.write(frame, moving(k as u32, width, height));
    }
    uploader.settle();
    drop(warm);
    let (mut count, mut bytes) = (0, 0);
    let start = Instant::now();
    for index in 0..frames {
        let frame = encoder.pool().acquire();
        encoder
            .submit(EncoderFrame::D3d12(frame), Rational::new(index as i64, 60))
            .unwrap();
        for packet in encoder.drain().unwrap() {
            count += 1;
            bytes += packet.data.len();
        }
    }
    for packet in encoder.flush().unwrap() {
        count += 1;
        bytes += packet.data.len();
    }
    let seconds = start.elapsed().as_secs_f64();
    assert_eq!(count, frames);
    (seconds, bytes)
}

#[test]
#[ignore = "run with --ignored --nocapture: 600 frames per cell, about three minutes"]
fn throughput_at_1080p_one_and_two_sessions() {
    let _engine = engine();
    let device = nvidia_device().unwrap();
    let frames = 600;
    println!("codec tuning preset sessions per_session_fps total_fps mbps_at_60");
    for tuning in [Tuning::HighQuality, Tuning::LowLatency] {
        for codec in [Codec::H264, Codec::Hevc, Codec::Av1] {
            for preset in [Preset::P1, Preset::P4, Preset::P7] {
                for sessions in [1usize, 2] {
                    let runs: Vec<(f64, usize)> = std::thread::scope(|scope| {
                        (0..sessions)
                            .map(|_| {
                                scope.spawn(|| session(&device, codec, preset, tuning, frames))
                            })
                            .collect::<Vec<_>>()
                            .into_iter()
                            .map(|handle| handle.join().unwrap())
                            .collect()
                    });
                    let per_session =
                        runs.iter().map(|(s, _)| frames as f64 / s).sum::<f64>() / sessions as f64;
                    let longest = runs.iter().map(|(s, _)| *s).fold(0.0, f64::max);
                    let total = (frames * sessions) as f64 / longest;
                    let mbps = runs[0].1 as f64 * 8.0 / frames as f64 * 60.0 / 1e6;
                    println!(
                        "{codec:?} {tuning:?} {preset:?} {sessions} {per_session:.1} {total:.1} {mbps:.1}"
                    );
                    assert!(
                        per_session >= 60.0,
                        "{codec:?} {preset:?} runs below real time"
                    );
                }
            }
        }
    }
}
