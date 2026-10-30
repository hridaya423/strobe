#![cfg(windows)]

use std::time::Instant;

use anyhow::Result;
use vrf_encode::d3d12ve::{D3d12H264Encoder, video_devices};
use vrf_encode::{
    Codec, Encoder, EncoderConfig, EncoderFrame, Packet, Quality, Rational, StreamInfo,
};
use windows::Win32::Graphics::Direct3D12::{
    D3D12_MESSAGE, D3D12GetDebugInterface, ID3D12Debug, ID3D12Device, ID3D12InfoQueue,
};
use windows::core::Interface;

#[allow(dead_code)]
mod video_support;

use video_support::*;

fn config(width: u32, height: u32, quality: Quality) -> EncoderConfig {
    let mut config = EncoderConfig::new(Codec::H264, width, height, Rational::new(30000, 1001));
    config.keyframe_interval = 30;
    config.quality = quality;
    config
}

fn encode(
    device: &ID3D12Device,
    config: EncoderConfig,
    frames: usize,
    fill: impl Fn(usize) -> Box<dyn FnOnce(&mut Planes)>,
) -> Result<(Vec<Packet>, StreamInfo)> {
    let mut encoder = D3d12H264Encoder::new(device, &config, 4)?;
    let mut uploader = Uploader::new(device, config.width, config.height, encoder.pool().format());
    let mut packets = Vec::new();
    for index in 0..frames {
        let mut frame = encoder.pool().acquire();
        uploader.write(&mut frame, fill(index));
        encoder.submit(EncoderFrame::D3d12(frame), pts_of(index, config.frame_rate))?;
        packets.extend(encoder.drain()?);
    }
    packets.extend(encoder.flush()?);
    let info = encoder.finish()?;
    uploader.settle();
    Ok((packets, info))
}

fn decoded_luma(file: &std::path::Path, width: usize, height: usize) -> Vec<Vec<u8>> {
    run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-i",
            file.to_str().unwrap(),
            "-vf",
            "extractplanes=y",
            "-pix_fmt",
            "gray",
            "-f",
            "rawvideo",
            "-",
        ],
    )
    .chunks_exact(width * height)
    .map(<[u8]>::to_vec)
    .collect()
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let squares: u64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| (i64::from(*x) - i64::from(*y)).pow(2) as u64)
        .sum();
    if squares == 0 {
        return 99.0;
    }
    10.0 * (255.0f64 * 255.0 * a.len() as f64 / squares as f64).log10()
}

fn debug_messages(device: &ID3D12Device) -> String {
    let Ok(queue) = device.cast::<ID3D12InfoQueue>() else {
        return "no info queue".into();
    };
    let mut out = String::new();
    unsafe {
        for index in 0..queue.GetNumStoredMessages() {
            let mut length = 0;
            let _ = queue.GetMessage(index, None, &mut length);
            let mut raw = vec![0u8; length];
            let message = raw.as_mut_ptr().cast::<D3D12_MESSAGE>();
            if queue.GetMessage(index, Some(message), &mut length).is_ok() {
                let text = std::slice::from_raw_parts(
                    (*message).pDescription,
                    (*message).DescriptionByteLength.saturating_sub(1),
                );
                out.push_str(&String::from_utf8_lossy(text));
                out.push('\n');
            }
        }
    }
    out
}

fn devices() -> Vec<(String, ID3D12Device)> {
    if std::env::var_os("VRF_D3D12_DEBUG").is_some() {
        let mut debug: Option<ID3D12Debug> = None;
        unsafe {
            D3D12GetDebugInterface(&mut debug).unwrap();
            debug.unwrap().EnableDebugLayer();
        }
    }
    let devices = video_devices().unwrap();
    assert!(!devices.is_empty(), "no adapter encodes H.264 on D3D12");
    devices
}

#[test]
fn every_adapter_encodes_60_frames_and_frame_37_decodes_as_37() {
    for (name, device) in devices() {
        let frame_rate = Rational::new(30000, 1001);
        let (packets, info) = encode(
            &device,
            config(WIDTH as u32, HEIGHT as u32, Quality::Balanced),
            FRAMES,
            |index| Box::new(flat(index)),
        )
        .unwrap_or_else(|error| panic!("{name}: {error:#}\n{}", debug_messages(&device)));
        assert_eq!(packets.len(), FRAMES, "{name}");
        let keyframes: Vec<usize> = (0..FRAMES).filter(|i| packets[*i].keyframe).collect();
        assert_eq!(keyframes, [0, 30], "{name}");
        for (index, packet) in packets.iter().enumerate() {
            assert!(same(packet.pts, pts_of(index, frame_rate)), "{name}");
            assert_eq!(packet.dts, packet.pts, "{name}");
            assert!(!packet.data.is_empty(), "{name}");
        }
        assert_eq!(info.codec, Codec::H264);
        assert!(!info.config.is_empty());

        let scratch = Scratch::new("d3d12ve-flat");
        let file = scratch.path("out.mp4");
        write_mp4(&file, packets, &info, frame_rate);
        assert_eq!(
            probe(&file, "stream=codec_name,width,height,nb_read_frames"),
            format!("h264,{WIDTH},{HEIGHT},60"),
            "{name}"
        );
        let indexes = decoded_indexes(&file, false);
        assert_eq!(indexes, (0..FRAMES as i64).collect::<Vec<_>>(), "{name}");
    }
}

#[test]
fn predicted_frames_decode_to_the_source_picture() {
    for (name, device) in devices() {
        let frame_rate = Rational::new(30000, 1001);
        let (packets, info) = encode(
            &device,
            config(WIDTH as u32, HEIGHT as u32, Quality::High),
            FRAMES,
            |index| Box::new(moving(index as u32, WIDTH, HEIGHT)),
        )
        .unwrap_or_else(|error| panic!("{name}: {error:#}"));
        let bytes: usize = packets.iter().map(|p| p.data.len()).sum();
        let scratch = Scratch::new("d3d12ve-moving");
        let file = scratch.path("out.mp4");
        write_mp4(&file, packets, &info, frame_rate);
        let decoded = decoded_luma(&file, WIDTH, HEIGHT);
        assert_eq!(decoded.len(), FRAMES, "{name}");
        let mut worst = f64::MAX;
        for (index, luma) in decoded.iter().enumerate() {
            let mut expected = vec![0u8; WIDTH * HEIGHT];
            let mut chroma = vec![0u8; WIDTH * HEIGHT / 2];
            moving(index as u32, WIDTH, HEIGHT)(&mut Planes {
                luma: &mut expected,
                luma_pitch: WIDTH,
                chroma: &mut chroma,
                chroma_pitch: WIDTH,
                ten_bit: false,
            });
            worst = worst.min(psnr(luma, &expected));
        }
        println!(
            "{name}: worst luma PSNR {worst:.1} dB, {:.1} kb/frame",
            bytes as f64 / FRAMES as f64 / 125.0
        );
        assert!(worst > 30.0, "{name}: worst PSNR {worst:.1} dB");
    }
}

fn ceiling(device: &ID3D12Device, width: u32, height: u32, frames: usize) -> Result<f64> {
    let config = config(width, height, Quality::Balanced);
    let mut encoder = D3d12H264Encoder::new(device, &config, 4)?;
    let mut uploader = Uploader::new(device, width, height, encoder.pool().format());
    let held: Vec<_> = (0..4).map(|_| encoder.pool().acquire()).collect();
    for (index, mut frame) in held.into_iter().enumerate() {
        uploader.write(
            &mut frame,
            moving(index as u32, width as usize, height as usize),
        );
    }
    uploader.settle();
    let mut rates = Vec::new();
    for _ in 0..5 {
        let started = Instant::now();
        for index in 0..frames {
            let frame = encoder.pool().acquire();
            let pts = Rational::new(index as i64 * 1001, 30000);
            encoder.submit(EncoderFrame::D3d12(frame), pts)?;
            encoder.drain()?;
        }
        rates.push(frames as f64 / started.elapsed().as_secs_f64());
        encoder = D3d12H264Encoder::new(device, &config, 4)?;
        uploader = Uploader::new(device, width, height, encoder.pool().format());
        let held: Vec<_> = (0..4).map(|_| encoder.pool().acquire()).collect();
        for (index, mut frame) in held.into_iter().enumerate() {
            uploader.write(
                &mut frame,
                moving(index as u32, width as usize, height as usize),
            );
        }
        uploader.settle();
    }
    rates.sort_by(f64::total_cmp);
    Ok(rates[rates.len() / 2])
}

#[test]
#[ignore]
fn d3d12ve_ceilings() {
    for (name, device) in devices() {
        for (width, height) in [(1920, 1080), (3840, 2160)] {
            let fps = ceiling(&device, width, height, 300).unwrap();
            println!("RESULT D3D12VE H264 {width}x{height} adapter=\"{name}\" median_fps={fps:.1}");
        }
    }
}
