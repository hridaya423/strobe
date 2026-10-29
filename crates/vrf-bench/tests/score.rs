use std::path::PathBuf;

use vrf_bench::score::{FrameScore, Rgb8, Sampling, compare, score};

fn lab(name: &str) -> Rgb8 {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/research/2026-10-29/metric-lab")
        .join(name);
    Rgb8::load_png(&p).unwrap()
}

fn against_default(name: &str) -> FrameScore {
    compare(&lab("a_default.png"), &lab(name)).unwrap()
}

fn close(actual: f64, expected: f64, tol: f64) {
    assert!(
        (actual - expected).abs() <= tol,
        "{actual} is not within {tol} of {expected}"
    );
}

#[test]
fn identical_images_score_zero() {
    let s = against_default("a_default.png");
    close(s.flip_mean, 0.0, 1e-6);
    assert_eq!(s.max_delta, 0);
    assert_eq!(s.frac_over_8, 0.0);
    close(s.ssim, 1.0, 1e-9);
}

#[test]
fn subpixel_shift_is_caught() {
    let s = against_default("v_shift.png");
    close(s.flip_mean, 0.05171, 0.005);
    assert_eq!(s.max_delta, 176);
    close(s.p999_delta, 116.0, 0.5);
    close(s.frac_over_8, 0.07456, 0.00001);
    close(s.frac_over_32, 0.04811, 0.00001);
    close(s.ssim, 0.9696, 0.0001);
}

#[test]
fn shadow_blur_change_stays_small() {
    let s = against_default("v_blur.png");
    assert_eq!(s.max_delta, 2);
    assert_eq!(s.frac_over_8, 0.0);
    close(s.ssim, 0.9987, 0.0001);
    close(s.flip_mean, 0.00357, 0.0004);
}

#[test]
fn radius_and_letter_spacing_tweak_hits_the_tail() {
    let s = against_default("v_tweak.png");
    assert_eq!(s.max_delta, 222);
    close(s.p999_delta, 220.0, 0.5);
    close(s.frac_over_32, 0.02381, 0.00001);
    close(s.flip_mean, 0.02789, 0.0028);
}

#[test]
fn geometric_precision_text_differs() {
    let s = against_default("v_geo.png");
    assert_eq!(s.max_delta, 199);
    close(s.flip_mean, 0.06159, 0.0062);
    close(s.ssim, 0.9843, 0.0001);
}

#[test]
fn mismatched_sizes_are_an_error() {
    let a = Rgb8 {
        width: 2,
        height: 2,
        data: vec![0; 12],
    };
    let b = Rgb8 {
        width: 2,
        height: 1,
        data: vec![0; 6],
    };
    assert!(compare(&a, &b).is_err());
}

#[test]
fn scores_a_video_against_png_frames() {
    let dir = std::env::temp_dir().join(format!("vrf-score-{}", std::process::id()));
    let pngs = dir.join("pngs");
    std::fs::create_dir_all(&pngs).unwrap();
    let video = dir.join("v.mkv");
    let st = std::process::Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=64x48:rate=10:duration=1",
        ])
        .args(["-c:v", "ffv1", "-pix_fmt", "bgr0"])
        .arg(&video)
        .status()
        .unwrap();
    assert!(st.success());
    let st = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(&video)
        .arg(pngs.join("%04d.png"))
        .status()
        .unwrap();
    assert!(st.success());

    let heat = dir.join("heat.png");
    let r = score(&video, &pngs, Sampling::Every(3), Some(&heat)).unwrap();
    assert_eq!(
        r.per_frame.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
        vec![0, 3, 6, 9]
    );
    assert_eq!(r.worst.max_delta.value, 0.0);
    assert!(heat.is_file());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn different_frame_counts_are_an_error() {
    let dir = std::env::temp_dir().join(format!("vrf-score-len-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let make = |name: &str, secs: &str| {
        let p = dir.join(name);
        let st = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("testsrc2=size=32x32:rate=10:duration={secs}"))
            .args(["-c:v", "ffv1"])
            .arg(&p)
            .status()
            .unwrap();
        assert!(st.success());
        p
    };
    let (long, short) = (make("long.mkv", "1"), make("short.mkv", "0.5"));
    let err = score(&long, &short, Sampling::All, None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("short.mkv has only 5 frames"), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();
}
