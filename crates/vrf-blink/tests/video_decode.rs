mod clips;

use std::process::Command;
use std::sync::Arc;

use anyhow::Result;
use clips::{clip, frame_id, packet_pts};
use vrf_blink::video::{Clip, Frame, Matrix, Reader, Stream, mp4};

const NAMES: [&str; 4] = [
    "cfr1080.mp4",
    "vfr1080.mp4",
    "off1080.mp4",
    "cfr1080-hevc.mp4",
];

#[test]
fn the_pts_table_is_what_ffprobe_reports() -> Result<()> {
    for name in NAMES {
        let path = clip(name)?;
        let decoded = Clip::open(&path)?;
        assert_eq!(decoded.pts, packet_pts(&path)?, "{name}");
    }
    Ok(())
}

#[test]
fn a_reader_started_at_any_frame_yields_that_frame_first() -> Result<()> {
    for name in NAMES {
        let decoded = Clip::open(&clip(name)?)?;
        let last = decoded.pts.len() - 1;
        for from in [0, 1, 7, last / 2, last - 1, last] {
            let mut reader = Reader::open(&decoded, from)?;
            let mut expected = from;
            while let Some((index, frame)) = reader.next(&decoded)? {
                assert_eq!(index, expected, "{name} from {from}");
                assert_eq!(
                    frame_id(&frame, decoded.width, decoded.height) as usize,
                    index,
                    "{name} from {from}"
                );
                expected += 1;
            }
            assert_eq!(expected, decoded.pts.len(), "{name} from {from}");
        }
    }
    Ok(())
}

#[test]
fn a_stream_never_hands_out_the_wrong_frame_when_jumping() -> Result<()> {
    for name in NAMES {
        let decoded = Arc::new(Clip::open(&clip(name)?)?);
        let (width, height) = (decoded.width, decoded.height);
        let count = decoded.pts.len();
        let stream = Stream::new(decoded);
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut at = 0;
        let mut visited = vec![];
        for step in 0..400 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            at = match seed % 10 {
                0..=5 => (at + 1) % count,
                6 => (at + count - 1) % count,
                7 => (at + 2 + (seed >> 8) as usize % 5) % count,
                8 => (seed >> 16) as usize % count,
                _ => at,
            };
            let frame = stream.get(at, 3)?;
            assert_eq!(
                frame_id(&frame, width, height) as usize,
                at,
                "{name} step {step}"
            );
            visited.push(at);
        }
        assert!(
            visited.windows(2).any(|pair| pair[1] < pair[0]),
            "{name} went backwards"
        );
    }
    Ok(())
}

#[test]
fn a_sequential_run_decodes_ahead_on_one_reader() -> Result<()> {
    let decoded = Arc::new(Clip::open(&clip("cfr1080.mp4")?)?);
    let stream = Stream::new(decoded.clone());
    for index in 0..decoded.pts.len() {
        stream.get(index, 3)?;
    }
    let (_, restarts) = stream.counters();
    assert_eq!(restarts, 1);
    Ok(())
}

#[test]
fn the_matrix_and_range_come_from_the_tags() -> Result<()> {
    let bt709 = Clip::open(&clip("cfr1080.mp4")?)?;
    let bt601 = Clip::open(&clip("cfr1080-601.mp4")?)?;
    let full = Clip::open(&clip("cfr1080-full.mp4")?)?;
    assert_eq!((bt709.matrix, bt709.full_range), (Matrix::Bt709, false));
    assert_eq!((bt601.matrix, bt601.full_range), (Matrix::Bt601, false));
    assert_eq!((full.matrix, full.full_range), (Matrix::Bt601, true));
    for decoded in [bt709, bt601, full] {
        assert_eq!(decoded.refusal(), None);
    }
    Ok(())
}

#[test]
fn the_container_table_is_the_decoders_and_ffprobes() -> Result<()> {
    for name in NAMES {
        let path = clip(name)?;
        let track = mp4::read(&path)?;
        let decoded = Clip::open(&path)?;
        assert_eq!(track.pts, packet_pts(&path)?, "{name}");
        assert_eq!(track.pts, decoded.pts, "{name}");
        assert_eq!(track.keyframes, decoded.keyframes, "{name}");
        assert_eq!(
            (track.width, track.height, track.timescale),
            (decoded.width, decoded.height, decoded.timescale),
            "{name}"
        );
    }
    Ok(())
}

#[test]
fn a_clip_in_another_container_is_not_read_as_mp4() -> Result<()> {
    let error = mp4::read(&clip("cfr1080-vp9.webm")?).unwrap_err();
    assert!(format!("{error:#}").contains("not an MP4"), "{error:#}");
    Ok(())
}

#[test]
fn a_decoded_picture_is_the_nv12_ffmpeg_decodes() -> Result<()> {
    for name in NAMES {
        let path = clip(name)?;
        let decoded = Clip::open(&path)?;
        let last = decoded.pts.len() - 1;
        for index in [0, 1, 29, last] {
            let output = Command::new("ffmpeg")
                .args(["-v", "error", "-i"])
                .arg(&path)
                .args(["-fps_mode", "passthrough", "-vf"])
                .arg(format!("select=eq(n\\,{index})"))
                .args(["-frames:v", "1", "-pix_fmt", "nv12", "-f", "rawvideo", "-"])
                .output()?;
            assert!(output.status.success(), "{name}: ffmpeg failed");
            let (_, frame) = Reader::open(&decoded, index)?
                .next(&decoded)?
                .expect("the frame exists");
            let mut ours = vec![0; Frame::nv12_len(decoded.width, decoded.height)];
            frame.write_nv12(&mut ours);
            assert!(
                ours == output.stdout,
                "{name} frame {index} differs from ffmpeg's"
            );
        }
    }
    Ok(())
}

#[cfg(windows)]
#[test]
fn hardware_and_software_decoding_give_the_same_pictures() -> Result<()> {
    let pictures = |decoded: &Clip, hardware| -> Result<Vec<Vec<u8>>> {
        vrf_blink::video::set_hardware_decoding(hardware);
        let mut reader = Reader::open(decoded, 0)?;
        let mut all = Vec::new();
        while let Some((_, frame)) = reader.next(decoded)? {
            let mut planes = vec![0; Frame::nv12_len(decoded.width, decoded.height)];
            frame.write_nv12(&mut planes);
            all.push(planes);
        }
        Ok(all)
    };
    for name in NAMES {
        let decoded = Clip::open(&clip(name)?)?;
        let hardware = pictures(&decoded, true)?;
        let software = pictures(&decoded, false)?;
        vrf_blink::video::set_hardware_decoding(true);
        assert_eq!(hardware.len(), decoded.pts.len(), "{name}");
        assert!(hardware == software, "{name}");
    }
    Ok(())
}
