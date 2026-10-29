use std::path::PathBuf;

use vrf_bench::corpus::{Corpus, Framework};
use vrf_bench::refs;
use vrf_bench::score::{Sampling, png_frames};

#[test]
fn renders_references_twice_and_records_the_noise_floor() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let corpus = Corpus {
        id: "hf-tiny".into(),
        root: fixtures,
        framework: Framework::Hyperframes,
        source: "hf-tiny".into(),
        entry: "index.html".into(),
        composition: None,
        width: 320,
        height: 180,
        fps: 10,
        frames: 10,
        stresses: vec![],
    };
    let out = std::env::temp_dir().join(format!("vrf-refs-{}", std::process::id()));
    let floor = refs::build(&corpus, Sampling::All, &out).unwrap();

    let frames = png_frames(&out.join("frames")).unwrap();
    assert_eq!(
        frames.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
        (0..10).collect::<Vec<_>>()
    );
    assert!(floor.worst.flip_mean.value.is_finite());
    assert!(out.join("noise_floor.json").is_file());
    assert!(!out.join("run2").exists());
    std::fs::remove_dir_all(&out).unwrap();
}
