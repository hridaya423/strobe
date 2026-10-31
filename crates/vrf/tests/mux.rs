use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};
use vrf::mux::{AudioTrack, Codec, Colour, Container, Muxer, Sample, StreamConfig};
#[cfg(target_os = "macos")]
use vrf_encode::audiotoolbox::encode_aac;
#[cfg(windows)]
use vrf_encode::mediafoundation::encode_aac;

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("mux-{name}"));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Dir(path)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(program: &str, args: &[&str]) -> String {
    let out = Command::new(program).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "{program} {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn ffmpeg(args: &[&str]) {
    let mut full = vec!["-v", "error", "-y"];
    full.extend_from_slice(args);
    run("ffmpeg", &full);
}

fn source(frames: &str, rate: &str) -> Vec<String> {
    [
        "-f",
        "lavfi",
        "-i",
        &format!("testsrc2=size=320x192:rate={rate}"),
        "-frames:v",
        frames,
    ]
    .map(String::from)
    .to_vec()
}

fn encode(out: &Path, frames: u32, rate: &str, codec_args: &[&str]) {
    let input = source(&frames.to_string(), rate);
    let mut args: Vec<&str> = input.iter().map(String::as_str).collect();
    args.extend_from_slice(codec_args);
    args.extend_from_slice(&["-video_track_timescale", "30"]);
    args.push(out.to_str().unwrap());
    ffmpeg(&args);
}

const X264: &[&str] = &[
    "-c:v",
    "libx264",
    "-preset",
    "veryfast",
    "-x264-params",
    "open-gop=0:bframes=0",
    "-g",
    "30",
];
const X264_B: &[&str] = &[
    "-c:v",
    "libx264",
    "-preset",
    "veryfast",
    "-x264-params",
    "open-gop=0:bframes=3",
    "-g",
    "30",
];
const X265_B: &[&str] = &[
    "-c:v",
    "libx265",
    "-preset",
    "ultrafast",
    "-x265-params",
    "open-gop=0:bframes=4:log-level=error",
    "-tag:v",
    "hvc1",
    "-g",
    "30",
];
const PRORES: &[&str] = &[
    "-c:v",
    "prores_ks",
    "-profile:v",
    "3",
    "-pix_fmt",
    "yuv422p10le",
];

fn probe(file: &Path, args: &[&str]) -> Vec<HashMap<String, String>> {
    let mut full = vec!["-v", "error", "-of", "compact=p=0"];
    full.extend_from_slice(args);
    full.push(file.to_str().unwrap());
    run("ffprobe", &full)
        .lines()
        .map(|line| {
            line.split('|')
                .filter_map(|kv| kv.split_once('='))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        })
        .collect()
}

fn stream(file: &Path) -> HashMap<String, String> {
    probe(
        file,
        &[
            "-count_frames",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,codec_tag_string,width,height,time_base,avg_frame_rate,\
             color_range,color_space,color_transfer,color_primaries,nb_read_frames",
        ],
    )
    .remove(0)
}

fn frame_pts(file: &Path) -> Vec<i64> {
    probe(
        file,
        &["-select_streams", "v:0", "-show_entries", "frame=pts"],
    )
    .iter()
    .map(|f| f["pts"].parse().unwrap())
    .collect()
}

fn frame_md5s(file: &Path) -> Vec<String> {
    run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-i",
            file.to_str().unwrap(),
            "-pix_fmt",
            "nv12",
            "-f",
            "framemd5",
            "-",
        ],
    )
    .lines()
    .filter(|l| !l.starts_with('#'))
    .map(|l| l.rsplit(", ").next().unwrap().to_string())
    .collect()
}

fn decodes_cleanly(file: &Path) {
    let out = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-i",
            file.to_str().unwrap(),
            "-f",
            "null",
            "-",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && stderr.is_empty(), "{stderr}");
}

fn children(mut b: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    while b.len() >= 8 {
        let size = u32::from_be_bytes(b[..4].try_into().unwrap()) as usize;
        let typ: [u8; 4] = b[4..8].try_into().unwrap();
        let (header, size) = match size {
            1 => (
                16,
                u64::from_be_bytes(b[8..16].try_into().unwrap()) as usize,
            ),
            0 => (8, b.len()),
            n => (8, n),
        };
        out.push((typ, &b[header..size]));
        b = &b[size..];
    }
    out
}

fn dig<'a>(mut b: &'a [u8], path: &[&str]) -> Option<&'a [u8]> {
    for name in path {
        b = children(b)
            .into_iter()
            .find(|(t, _)| t == name.as_bytes())?
            .1;
    }
    Some(b)
}

const STBL: [&str; 5] = ["moov", "trak", "mdia", "minf", "stbl"];

fn stbl_box<'a>(file: &'a [u8], name: &str) -> Option<&'a [u8]> {
    let mut path = STBL.to_vec();
    path.push(name);
    dig(file, &path)
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}

fn top_level(file: &[u8]) -> Vec<String> {
    children(file)
        .iter()
        .map(|(t, _)| String::from_utf8_lossy(t).into_owned())
        .collect()
}

fn config_record(bytes: &[u8], fourcc: &[u8; 4]) -> Vec<u8> {
    let at = bytes
        .windows(4)
        .position(|w| w == fourcc)
        .expect("config box");
    let size = u32_at(bytes, at - 4) as usize;
    bytes[at + 4..at - 4 + size].to_vec()
}

fn read_chunk(path: &Path, codec: Codec) -> Result<(StreamConfig, Vec<Sample>)> {
    let bytes = fs::read(path)?;
    let info = stream(path);
    let config = match codec {
        Codec::H264 => config_record(&bytes, b"avcC"),
        Codec::Hevc => config_record(&bytes, b"hvcC"),
        _ => Vec::new(),
    };
    let samples = probe(
        path,
        &[
            "-select_streams",
            "v:0",
            "-show_entries",
            "packet=pts,dts,duration,size,pos,flags",
        ],
    )
    .iter()
    .map(|p| -> Result<Sample> {
        let pos: usize = p["pos"].parse()?;
        let size: usize = p["size"].parse()?;
        ensure!(pos + size <= bytes.len());
        Ok(Sample {
            data: bytes[pos..pos + size].to_vec(),
            pts: p["pts"].parse()?,
            dts: p["dts"].parse()?,
            duration: p["duration"].parse()?,
            keyframe: p["flags"].starts_with('K'),
        })
    })
    .collect::<Result<_>>()
    .context("packets")?;
    let config = StreamConfig {
        codec,
        width: info["width"].parse()?,
        height: info["height"].parse()?,
        colour: Colour::BT709,
        config,
    };
    Ok((config, samples))
}

fn retimed(samples: &[Sample], first: i64, duration: u32) -> Vec<Sample> {
    samples
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let t = first + i as i64 * duration as i64;
            Sample {
                data: s.data.clone(),
                pts: t,
                dts: t,
                duration,
                keyframe: s.keyframe,
            }
        })
        .collect()
}

fn mux(
    out: &Path,
    container: Container,
    timescale: u32,
    chunks: &[(StreamConfig, Vec<Sample>)],
) -> Result<()> {
    let mut muxer = Muxer::create(out, container, timescale)?;
    for (config, samples) in chunks {
        muxer.start_chunk(config.clone())?;
        for sample in samples {
            muxer.write(sample)?;
        }
    }
    muxer.finish()
}

#[test]
fn sixty_frames_at_30_fps_have_exact_pts_and_bt709_tags() {
    let dir = Dir::new("h264-30");
    let src = dir.file("src.mp4");
    encode(&src, 60, "30", X264);
    let before = stream(&src);
    assert_eq!(
        before["color_space"], "unknown",
        "fixture must carry no colour tags"
    );

    let (config, samples) = read_chunk(&src, Codec::H264).unwrap();
    assert_eq!(samples.len(), 60);
    let out = dir.file("out.mp4");
    mux(
        &out,
        Container::Mp4,
        30,
        &[(config, retimed(&samples, 0, 1))],
    )
    .unwrap();

    let s = stream(&out);
    assert_eq!(s["codec_name"], "h264");
    assert_eq!(s["codec_tag_string"], "avc1");
    assert_eq!(s["nb_read_frames"], "60");
    assert_eq!(s["time_base"], "1/30");
    assert_eq!(s["avg_frame_rate"], "30/1");
    assert_eq!((s["width"].as_str(), s["height"].as_str()), ("320", "192"));
    assert_eq!(
        (
            s["color_space"].as_str(),
            s["color_transfer"].as_str(),
            s["color_primaries"].as_str(),
            s["color_range"].as_str()
        ),
        ("bt709", "bt709", "bt709", "tv")
    );
    assert_eq!(frame_pts(&out), (0..60).collect::<Vec<_>>());
    decodes_cleanly(&out);
    assert_eq!(frame_md5s(&out), frame_md5s(&src));
}

#[test]
fn colour_tags_follow_the_stream_config() {
    let dir = Dir::new("colour");
    let src = dir.file("src.mp4");
    encode(&src, 8, "30", X264);
    let (mut config, samples) = read_chunk(&src, Codec::H264).unwrap();
    config.colour = Colour {
        primaries: 6,
        transfer: 6,
        matrix: 6,
        full_range: true,
    };
    let out = dir.file("out.mp4");
    mux(
        &out,
        Container::Mp4,
        30,
        &[(config, retimed(&samples, 0, 1))],
    )
    .unwrap();
    let s = stream(&out);
    assert_eq!(
        (
            s["color_space"].as_str(),
            s["color_transfer"].as_str(),
            s["color_primaries"].as_str(),
            s["color_range"].as_str()
        ),
        ("smpte170m", "smpte170m", "smpte170m", "pc")
    );
}

#[test]
fn ntsc_timebase_24000_1001_is_exact() {
    let dir = Dir::new("ntsc");
    let src = dir.file("src.mp4");
    encode(&src, 48, "30", X264);
    let (config, samples) = read_chunk(&src, Codec::H264).unwrap();
    let out = dir.file("out.mp4");
    mux(
        &out,
        Container::Mp4,
        24000,
        &[(config, retimed(&samples, 0, 1001))],
    )
    .unwrap();

    let s = stream(&out);
    assert_eq!(s["time_base"], "1/24000");
    assert_eq!(s["avg_frame_rate"], "24000/1001");
    assert_eq!(s["nb_read_frames"], "48");
    assert_eq!(
        frame_pts(&out),
        (0..48).map(|i| i * 1001).collect::<Vec<_>>()
    );
    decodes_cleanly(&out);
}

fn b_frames_keep_presentation_times(name: &str, codec: Codec, args: &[&str], tag: &str) {
    let dir = Dir::new(name);
    let src = dir.file("src.mp4");
    encode(&src, 60, "30", args);
    let (config, samples) = read_chunk(&src, codec).unwrap();
    assert!(
        samples.iter().any(|s| s.pts != s.dts),
        "fixture must reorder frames"
    );
    assert!(samples[0].dts < 0, "fixture dts starts before pts");
    let out = dir.file("out.mp4");
    mux(&out, Container::Mp4, 30, &[(config, samples)]).unwrap();

    let s = stream(&out);
    assert_eq!(s["codec_tag_string"], tag);
    assert_eq!(s["nb_read_frames"], "60");
    assert_eq!(frame_pts(&out), (0..60).collect::<Vec<_>>());
    decodes_cleanly(&out);
    assert_eq!(frame_md5s(&out), frame_md5s(&src));
    #[cfg(target_os = "macos")]
    assert_eq!(
        avfoundation_md5s(&out, &dir, "320", "192"),
        frame_md5s(&out)
    );
}

#[test]
fn h264_with_b_frames_keeps_presentation_times() {
    b_frames_keep_presentation_times("h264-b", Codec::H264, X264_B, "avc1");
}

#[test]
fn hevc_with_b_frames_keeps_presentation_times() {
    b_frames_keep_presentation_times("hevc-b", Codec::Hevc, X265_B, "hvc1");
}

#[test]
fn hevc_reports_its_codec() {
    let dir = Dir::new("hevc");
    let src = dir.file("src.mp4");
    encode(&src, 60, "30", X265_B);
    let (config, samples) = read_chunk(&src, Codec::Hevc).unwrap();
    let out = dir.file("out.mp4");
    mux(&out, Container::Mp4, 30, &[(config, samples)]).unwrap();
    let s = stream(&out);
    assert_eq!(s["codec_name"], "hevc");
    assert_eq!(s["nb_read_frames"], "60");
    assert_eq!(s["color_space"], "bt709");
}

#[test]
fn prores_in_mov_round_trips() {
    let dir = Dir::new("prores");
    let src = dir.file("src.mov");
    encode(&src, 24, "30", PRORES);
    let (config, samples) = read_chunk(&src, Codec::ProRes422Hq).unwrap();
    assert_eq!(samples.len(), 24);
    let out = dir.file("out.mov");
    mux(
        &out,
        Container::Mov,
        30,
        &[(config, retimed(&samples, 0, 1))],
    )
    .unwrap();

    let s = stream(&out);
    assert_eq!(s["codec_name"], "prores");
    assert_eq!(s["codec_tag_string"], "apch");
    assert_eq!(s["nb_read_frames"], "24");
    assert_eq!(frame_pts(&out), (0..24).collect::<Vec<_>>());
    let ftyp = fs::read(&out).unwrap();
    assert_eq!(&ftyp[8..12], b"qt  ");
    decodes_cleanly(&out);
    assert_eq!(frame_md5s(&out), frame_md5s(&src));
}

#[test]
fn output_is_faststart() {
    let dir = Dir::new("faststart");
    let src = dir.file("src.mp4");
    encode(&src, 30, "30", X264);
    let (config, samples) = read_chunk(&src, Codec::H264).unwrap();
    let out = dir.file("out.mp4");
    mux(
        &out,
        Container::Mp4,
        30,
        &[(config, retimed(&samples, 0, 1))],
    )
    .unwrap();
    let bytes = fs::read(&out).unwrap();
    assert_eq!(top_level(&bytes), ["ftyp", "moov", "mdat"]);

    let mdat_payload = bytes.len() - children(&bytes)[2].1.len();
    let stco = stbl_box(&bytes, "stco").unwrap();
    assert_eq!(u32_at(stco, 4), 1);
    assert_eq!(u32_at(stco, 8) as usize, mdat_payload);
    assert!(!dir.file("out.mp4.mdat").exists());
}

#[test]
fn keyframes_are_listed_in_stss() {
    let dir = Dir::new("stss");
    let src = dir.file("src.mp4");
    encode(&src, 60, "30", X264);
    let (config, samples) = read_chunk(&src, Codec::H264).unwrap();
    let keys: Vec<u32> = samples
        .iter()
        .enumerate()
        .filter(|(_, s)| s.keyframe)
        .map(|(i, _)| i as u32 + 1)
        .collect();
    assert_eq!(keys, [1, 31]);
    let out = dir.file("out.mp4");
    mux(&out, Container::Mp4, 30, &[(config, samples)]).unwrap();
    let bytes = fs::read(&out).unwrap();
    let stss = stbl_box(&bytes, "stss").unwrap();
    let listed: Vec<u32> = (0..u32_at(stss, 4) as usize)
        .map(|i| u32_at(stss, 8 + 4 * i))
        .collect();
    assert_eq!(listed, keys);
}

#[test]
fn wide_offsets_write_co64_and_a_large_mdat() {
    let dir = Dir::new("wide");
    let src = dir.file("src.mp4");
    encode(&src, 30, "30", X264);
    let (config, samples) = read_chunk(&src, Codec::H264).unwrap();
    let out = dir.file("out.mp4");
    let mut muxer = Muxer::create(&out, Container::Mp4, 30).unwrap();
    muxer.force_wide_offsets();
    muxer.start_chunk(config).unwrap();
    for sample in &samples {
        muxer.write(sample).unwrap();
    }
    muxer.finish().unwrap();

    let bytes = fs::read(&out).unwrap();
    assert_eq!(top_level(&bytes), ["ftyp", "moov", "mdat"]);
    assert!(stbl_box(&bytes, "stco").is_none());
    assert!(stbl_box(&bytes, "co64").is_some());
    assert_eq!(stream(&out)["nb_read_frames"], "30");
    decodes_cleanly(&out);
    assert_eq!(frame_md5s(&out), frame_md5s(&src));
}

fn two_chunks(dir: &Dir) -> [(StreamConfig, Vec<Sample>); 2] {
    let vt = dir.file("vt.mp4");
    let x264 = dir.file("x264.mp4");
    let vt_args: &[&str] = if cfg!(target_os = "macos") {
        &[
            "-c:v",
            "h264_videotoolbox",
            "-b:v",
            "4M",
            "-bf",
            "0",
            "-g",
            "30",
        ]
    } else {
        &[
            "-c:v",
            "libx264",
            "-profile:v",
            "baseline",
            "-preset",
            "veryfast",
            "-g",
            "30",
        ]
    };
    encode(&vt, 60, "30", vt_args);
    encode(&x264, 60, "30", X264);
    let (config_a, samples_a) = read_chunk(&vt, Codec::H264).unwrap();
    let (config_b, samples_b) = read_chunk(&x264, Codec::H264).unwrap();
    [
        (config_a, retimed(&samples_a, 0, 1)),
        (config_b, retimed(&samples_b, 60, 1)),
    ]
}

#[test]
fn chunks_from_two_encoders_get_one_stsd_entry_each() {
    let dir = Dir::new("stsd");
    let chunks = two_chunks(&dir);
    assert_ne!(chunks[0].0.config, chunks[1].0.config);
    let out = dir.file("out.mp4");
    mux(&out, Container::Mp4, 30, &chunks).unwrap();

    let bytes = fs::read(&out).unwrap();
    let stsd = stbl_box(&bytes, "stsd").unwrap();
    assert_eq!(u32_at(stsd, 4), 2);
    let entries = children(&stsd[8..]);
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|(t, _)| t == b"avc1"));
    let stsc = stbl_box(&bytes, "stsc").unwrap();
    assert_eq!(u32_at(stsc, 4), 2);
    let rows: Vec<[u32; 3]> = (0..2)
        .map(|i| [8 + 12 * i, 12 + 12 * i, 16 + 12 * i].map(|at| u32_at(stsc, at)))
        .collect();
    assert_eq!(rows, [[1, 60, 1], [2, 60, 2]]);
    assert_eq!(stream(&out)["nb_read_frames"], "120");
    assert_eq!(frame_pts(&out), (0..120).collect::<Vec<_>>());
}

#[test]
fn chunks_with_identical_config_share_one_entry() {
    let dir = Dir::new("shared");
    let src = dir.file("src.mp4");
    encode(&src, 60, "30", X264);
    let (config, samples) = read_chunk(&src, Codec::H264).unwrap();
    let first = retimed(&samples[..30], 0, 1);
    let second = retimed(&samples[..30], 30, 1);
    let out = dir.file("out.mp4");
    mux(
        &out,
        Container::Mp4,
        30,
        &[(config.clone(), first), (config, second)],
    )
    .unwrap();
    let bytes = fs::read(&out).unwrap();
    assert_eq!(u32_at(stbl_box(&bytes, "stsd").unwrap(), 4), 1);
    let stsc = stbl_box(&bytes, "stsc").unwrap();
    assert_eq!(u32_at(stsc, 4), 1);
    assert_eq!(stream(&out)["nb_read_frames"], "60");
    decodes_cleanly(&out);
}

#[cfg(target_os = "macos")]
fn avfoundation_md5s(file: &Path, dir: &Dir, width: &str, height: &str) -> Vec<String> {
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/video_support/avdecode.swift");
    let raw = dir.file("av.nv12");
    let log = run(
        "swift",
        &[
            script.to_str().unwrap(),
            file.to_str().unwrap(),
            raw.to_str().unwrap(),
        ],
    );
    eprintln!("{log}");
    let size = format!("{width}x{height}");
    run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "nv12",
            "-s",
            &size,
            "-i",
            raw.to_str().unwrap(),
            "-f",
            "framemd5",
            "-",
        ],
    )
    .lines()
    .filter(|l| !l.starts_with('#'))
    .map(|l| l.rsplit(", ").next().unwrap().to_string())
    .collect()
}

#[cfg(target_os = "macos")]
#[test]
fn two_encoder_file_decodes_identically_in_ffmpeg_and_avfoundation() {
    let dir = Dir::new("stitch");
    let chunks = two_chunks(&dir);
    let out = dir.file("out.mp4");
    mux(&out, Container::Mp4, 30, &chunks).unwrap();

    let ffmpeg_md5 = frame_md5s(&out);
    assert_eq!(ffmpeg_md5.len(), 120);
    let mut separate = frame_md5s(&dir.file("vt.mp4"));
    separate.extend(frame_md5s(&dir.file("x264.mp4")));
    assert_eq!(
        ffmpeg_md5, separate,
        "stitched frames differ from the chunks decoded alone"
    );

    let av_md5 = avfoundation_md5s(&out, &dir, "320", "192");
    assert_eq!(av_md5, ffmpeg_md5);
    eprintln!("frame md5s:\n{}", ffmpeg_md5.join("\n"));
}

#[test]
fn timestamps_must_be_contiguous() {
    let dir = Dir::new("gap");
    let src = dir.file("src.mp4");
    encode(&src, 4, "30", X264);
    let (config, samples) = read_chunk(&src, Codec::H264).unwrap();
    let mut gapped = retimed(&samples, 0, 1);
    gapped[2].dts = 5;
    gapped[2].pts = 5;
    let err = mux(
        &dir.file("out.mp4"),
        Container::Mp4,
        30,
        &[(config, gapped)],
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("dts"), "{err}");
}

#[test]
fn prores_needs_a_mov_container() {
    let dir = Dir::new("prores-mp4");
    let config = StreamConfig {
        codec: Codec::ProRes422,
        width: 320,
        height: 192,
        colour: Colour::BT709,
        config: Vec::new(),
    };
    let mut muxer = Muxer::create(&dir.file("out.mp4"), Container::Mp4, 30).unwrap();
    assert!(muxer.start_chunk(config).is_err());
}

#[test]
fn mixed_codecs_and_sizes_are_rejected() {
    let dir = Dir::new("mixed");
    let src = dir.file("src.mp4");
    encode(&src, 2, "30", X264);
    let (config, _) = read_chunk(&src, Codec::H264).unwrap();
    let mut muxer = Muxer::create(&dir.file("out.mp4"), Container::Mp4, 30).unwrap();
    muxer.start_chunk(config.clone()).unwrap();
    let hevc = StreamConfig {
        codec: Codec::Hevc,
        ..config.clone()
    };
    assert!(muxer.start_chunk(hevc).is_err());
    let wider = StreamConfig {
        width: 640,
        ..config
    };
    assert!(muxer.start_chunk(wider).is_err());
}

#[test]
fn writing_before_a_chunk_starts_is_rejected() {
    let dir = Dir::new("no-chunk");
    let mut muxer = Muxer::create(&dir.file("out.mp4"), Container::Mp4, 30).unwrap();
    let sample = Sample {
        data: vec![0, 0, 0, 1, 9],
        pts: 0,
        dts: 0,
        duration: 1,
        keyframe: true,
    };
    assert!(muxer.write(&sample).is_err());
}

fn packet(pts: vrf_encode::Rational) -> vrf_encode::Packet {
    vrf_encode::Packet {
        data: vec![1],
        pts,
        dts: pts,
        keyframe: true,
    }
}

#[test]
fn encoder_packets_become_whole_ticks_of_the_timebase() {
    let timebase = vrf_encode::Rational::new(1, 24000);
    let sample = Sample::from_packet(
        packet(vrf_encode::Rational::new(5 * 1001, 24000)),
        timebase,
        1001,
    )
    .unwrap();
    assert_eq!(
        (sample.pts, sample.dts, sample.duration),
        (5005, 5005, 1001)
    );

    let half_tick = packet(vrf_encode::Rational::new(1, 48000));
    assert!(Sample::from_packet(half_tick, timebase, 1001).is_err());
}

#[test]
fn stream_info_becomes_a_bt709_stream_config() {
    let info = vrf_encode::StreamInfo {
        codec: vrf_encode::Codec::Hevc,
        width: 1920,
        height: 1080,
        colour: vrf_encode::ColourSpace::BT709,
        timebase: vrf_encode::Rational::new(1, 30),
        config: vec![1, 2, 3],
    };
    let config = StreamConfig::try_from(&info).unwrap();
    assert_eq!(
        config,
        StreamConfig {
            codec: Codec::Hevc,
            width: 1920,
            height: 1080,
            colour: Colour::BT709,
            config: vec![1, 2, 3],
        }
    );
}

const RATE: u32 = 48_000;
const CLICK_FRAMES: [u32; 4] = [30, 60, 90, 120];

fn click_track(seconds: usize) -> Vec<f32> {
    let mut samples = vec![0.0f32; seconds * RATE as usize * 2];
    for video_frame in CLICK_FRAMES {
        let start = video_frame as usize * (RATE as usize / 30);
        for i in 0..240 {
            let envelope = (std::f32::consts::PI * i as f32 / 240.0).sin().powi(2);
            let v =
                0.8 * envelope * (i as f32 * 2_000.0 * std::f32::consts::TAU / RATE as f32).sin();
            samples[(start + i) * 2] = v;
            samples[(start + i) * 2 + 1] = v;
        }
    }
    samples
}

fn decode_f32(file: &Path) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i", file.to_str().unwrap()])
        .args([
            "-map", "0:a:0", "-f", "f32le", "-ar", "48000", "-ac", "2", "-",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect()
}

fn centroid(stereo: &[f32], around: usize, half: usize) -> f64 {
    let (mut weight, mut moment) = (0.0, 0.0);
    for frame in around.saturating_sub(half)..(around + half).min(stereo.len() / 2) {
        let e = f64::from(stereo[frame * 2]).powi(2);
        weight += e;
        moment += e * frame as f64;
    }
    moment / weight
}

fn av_fixture(dir: &Dir, frames: u32) -> (StreamConfig, Vec<Sample>) {
    let src = dir.file("src.mp4");
    encode(&src, frames, "30", X264);
    let (config, samples) = read_chunk(&src, Codec::H264).unwrap();
    (config, retimed(&samples, 0, 1))
}

fn aac_track(samples: &[f32]) -> AudioTrack {
    let stream = encode_aac(
        samples,
        vrf_encode::AacConfig {
            sample_rate: RATE,
            channels: 2,
            bit_rate: 128_000,
        },
    )
    .unwrap();
    AudioTrack::from(&stream)
}

fn mux_with_audio(
    out: &Path,
    container: Container,
    video: (StreamConfig, Vec<Sample>),
    audio: AudioTrack,
) -> Result<()> {
    let mut muxer = Muxer::create(out, container, 30)?;
    muxer.start_chunk(video.0)?;
    for sample in &video.1 {
        muxer.write(sample)?;
    }
    muxer.set_audio(audio)?;
    muxer.finish()
}

fn audio_stream(file: &Path) -> HashMap<String, String> {
    probe(
        file,
        &[
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=codec_name,codec_tag_string,sample_rate,channels,start_time,duration",
        ],
    )
    .remove(0)
}

fn aac_round_trip(container: Container, name: &str) {
    let dir = Dir::new(name);
    let video = av_fixture(&dir, 150);
    let input = click_track(5);
    let out = dir.file(name);
    mux_with_audio(&out, container, video, aac_track(&input)).unwrap();

    let a = audio_stream(&out);
    assert_eq!(a["codec_name"], "aac");
    assert_eq!(a["codec_tag_string"], "mp4a");
    assert_eq!(a["sample_rate"], "48000");
    assert_eq!(a["channels"], "2");
    assert_eq!(a["start_time"].parse::<f64>().unwrap(), 0.0);
    assert_eq!(stream(&out)["nb_read_frames"], "150");
    decodes_cleanly(&out);

    let decoded = decode_f32(&out);
    assert_eq!(decoded.len(), input.len());
    let times = probe(
        &out,
        &["-select_streams", "v:0", "-show_entries", "frame=pts_time"],
    );
    for (k, video_frame) in CLICK_FRAMES.into_iter().enumerate() {
        let video_time: f64 = times[video_frame as usize]["pts_time"].parse().unwrap();
        let expected = video_time * f64::from(RATE) + 120.0;
        let found = centroid(&decoded, expected as usize, 2_048);
        eprintln!("click {k}: audio {found:.1}, video frame {expected:.1}");
        assert!(
            (found - expected).abs() < 1024.0,
            "click {k} is {} samples from its video frame",
            found - expected
        );
        assert!((found - expected).abs() < 64.0);
    }
}

#[cfg(any(target_os = "macos", windows))]
#[test]
fn aac_in_mp4_lands_clicks_on_their_video_frames() {
    aac_round_trip(Container::Mp4, "aac-mp4.mp4");
}

#[cfg(any(target_os = "macos", windows))]
#[test]
fn aac_in_mov_lands_clicks_on_their_video_frames() {
    aac_round_trip(Container::Mov, "aac-mov.mov");
}

#[cfg(target_os = "macos")]
fn avfoundation_audio(file: &Path, dir: &Dir) -> Vec<f32> {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audio_support/avaudio.swift");
    let raw = dir.file("av.f32");
    let log = run(
        "swift",
        &[
            script.to_str().unwrap(),
            file.to_str().unwrap(),
            raw.to_str().unwrap(),
        ],
    );
    eprintln!("{log}");
    fs::read(&raw)
        .unwrap()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect()
}

#[cfg(target_os = "macos")]
fn decoders_agree(container: Container, name: &str) {
    let dir = Dir::new(name);
    let video = av_fixture(&dir, 150);
    let out = dir.file(name);
    mux_with_audio(&out, container, video, aac_track(&click_track(5))).unwrap();

    let av = avfoundation_audio(&out, &dir);
    let ff = decode_f32(&out);
    assert_eq!(av.len(), ff.len());
    let worst = av
        .iter()
        .zip(&ff)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f32::max);
    assert!(worst < 2e-3, "worst sample difference {worst}");
}

#[cfg(target_os = "macos")]
#[test]
fn ffmpeg_and_avfoundation_decode_the_same_audio_in_mp4() {
    decoders_agree(Container::Mp4, "av-mp4.mp4");
}

#[cfg(target_os = "macos")]
#[test]
fn ffmpeg_and_avfoundation_decode_the_same_audio_in_mov() {
    decoders_agree(Container::Mov, "av-mov.mov");
}

#[cfg(target_os = "macos")]
#[test]
fn audio_edit_list_skips_exactly_the_priming() {
    let dir = Dir::new("elst");
    let video = av_fixture(&dir, 150);
    let input = click_track(5);
    let stream = vrf_encode::audiotoolbox::encode_aac(
        &input,
        vrf_encode::AacConfig {
            sample_rate: RATE,
            channels: 2,
            bit_rate: 128_000,
        },
    )
    .unwrap();
    let out = dir.file("out.mp4");
    mux_with_audio(&out, Container::Mp4, video, AudioTrack::from(&stream)).unwrap();

    let bytes = fs::read(&out).unwrap();
    let moov = dig(&bytes, &["moov"]).unwrap();
    let movie_timescale = u32_at(dig(moov, &["mvhd"]).unwrap(), 12);
    let traks: Vec<_> = children(moov)
        .into_iter()
        .filter(|(t, _)| t == b"trak")
        .collect();
    assert_eq!(traks.len(), 2);
    let elst = dig(traks[1].1, &["edts", "elst"]).unwrap();
    assert_eq!(u32_at(elst, 4), 1);
    let segment = u64::from(u32_at(elst, 8));
    assert_eq!(
        segment * u64::from(RATE),
        stream.frames * u64::from(movie_timescale)
    );
    assert_eq!(u32_at(elst, 12), stream.priming_frames);
    let mdhd = dig(traks[1].1, &["mdia", "mdhd"]).unwrap();
    assert_eq!(u32_at(mdhd, 12), RATE);
    assert_eq!(
        u64::from(u32_at(mdhd, 16)),
        stream.packets.len() as u64 * 1024
    );
}

#[cfg(target_os = "macos")]
#[test]
fn audio_and_video_chunks_alternate_by_time() {
    let dir = Dir::new("interleave");
    let video = av_fixture(&dir, 300);
    let out = dir.file("out.mp4");
    mux_with_audio(&out, Container::Mp4, video, aac_track(&click_track(10))).unwrap();

    let mut packets: Vec<(u64, f64)> = probe(&out, &["-show_entries", "packet=pos,pts_time"])
        .iter()
        .map(|p| (p["pos"].parse().unwrap(), p["pts_time"].parse().unwrap()))
        .collect();
    packets.sort_by_key(|p| p.0);
    let mut latest = f64::MIN;
    let mut worst_lag = 0.0f64;
    for (_, time) in packets {
        worst_lag = worst_lag.max(latest - time);
        latest = latest.max(time);
    }
    assert!(
        worst_lag < 1.01,
        "a packet sits {worst_lag} s behind the file position"
    );
}

#[test]
fn pcm_in_mov_round_trips_exactly() {
    let dir = Dir::new("pcm");
    let src = dir.file("src.mov");
    encode(&src, 60, "30", PRORES);
    let (config, samples) = read_chunk(&src, Codec::ProRes422Hq).unwrap();
    let input: Vec<f32> = (0..2 * RATE as usize * 2)
        .map(|i| ((i / 2) as f32 * 440.0 * std::f32::consts::TAU / RATE as f32).sin() * 0.9)
        .collect();
    let out = dir.file("out.mov");
    mux_with_audio(
        &out,
        Container::Mov,
        (config, retimed(&samples, 0, 1)),
        AudioTrack::pcm_s16le(&input, RATE, 2),
    )
    .unwrap();

    let a = audio_stream(&out);
    assert_eq!(a["codec_name"], "pcm_s16le");
    assert_eq!(a["codec_tag_string"], "sowt");
    assert_eq!(a["duration"].parse::<f64>().unwrap(), 2.0);
    decodes_cleanly(&out);
    let decoded = decode_f32(&out);
    assert_eq!(decoded.len(), input.len());
    for (d, i) in decoded.iter().zip(&input) {
        assert_eq!(*d, (i * 32768.0).round().clamp(-32768.0, 32767.0) / 32768.0);
    }
    #[cfg(target_os = "macos")]
    assert_eq!(avfoundation_audio(&out, &dir), decoded);
}

#[test]
fn pcm_is_only_allowed_in_mov() {
    let dir = Dir::new("pcm-mp4");
    let video = av_fixture(&dir, 30);
    let mut muxer = Muxer::create(&dir.file("out.mp4"), Container::Mp4, 30).unwrap();
    muxer.start_chunk(video.0).unwrap();
    assert!(
        muxer
            .set_audio(AudioTrack::pcm_s16le(&[0.0; 4], RATE, 2))
            .is_err()
    );
}
