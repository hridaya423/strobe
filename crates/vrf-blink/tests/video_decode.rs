mod clips;

use std::sync::Arc;

use anyhow::Result;
use clips::{clip, frame_id, packet_pts};
use vrf_blink::video::{Clip, Matrix, Reader, Stream};

const NAMES: [&str; 4] = ["cfr1080.mp4", "vfr1080.mp4", "off1080.mp4", "cfr1080-hevc.mp4"];

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
            assert_eq!(frame_id(&frame, width, height) as usize, at, "{name} step {step}");
            visited.push(at);
        }
        assert!(visited.windows(2).any(|pair| pair[1] < pair[0]), "{name} went backwards");
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
