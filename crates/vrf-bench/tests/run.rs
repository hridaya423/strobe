use vrf_bench::run::Timing;

#[test]
fn parses_hyperfine_wall_clock_summary() {
    let json = include_str!("fixtures/hyperfine.json");
    let t = Timing::from_hyperfine(json).unwrap();
    assert_eq!(t.median_s, 0.07710375);
    assert_eq!(t.min_s, 0.07284362500000001);
    assert_eq!(t.max_s, 0.112381042);
    assert_eq!(t.runs, 3);
    assert!((t.fps(300) - 300.0 / 0.07710375).abs() < 1e-9);
}

#[test]
fn rejects_a_failed_run() {
    let json =
        include_str!("fixtures/hyperfine.json").replacen("\"exit_code\": 0", "\"exit_code\": 1", 1);
    assert!(Timing::from_hyperfine(&json).is_err());
}

#[test]
fn vrf_engine_renders_both_frameworks() {
    use vrf_bench::corpus::{Framework, load};
    use vrf_bench::engines::Engine;

    let remotion = load("rm-transitions-video").unwrap();
    let hyperframes = load("hf-figma-launch").unwrap();
    assert!(Engine::Vrf.supports(Framework::Remotion));
    assert!(Engine::Vrf.supports(Framework::Hyperframes));
    let invocation = Engine::Vrf.invocation(&remotion, std::path::Path::new("/tmp/out.mp4"));
    assert!(invocation.argv[0].ends_with("target/release/vrf"));
    assert_eq!(
        invocation.argv[1..],
        [
            "render".to_string(),
            remotion.project_dir().display().to_string(),
            "--composition".into(),
            "Main".into(),
            "--entry".into(),
            "src/index.ts".into(),
            "-o".into(),
            "/tmp/out.mp4".into(),
        ]
    );
    assert_eq!(hyperframes.framework, Framework::Hyperframes);
    let invocation = Engine::Vrf.invocation(&hyperframes, std::path::Path::new("/tmp/out.mp4"));
    assert_eq!(invocation.argv[1], "render");
    assert_eq!(
        invocation.argv[2],
        hyperframes.project_dir().display().to_string()
    );
    assert_eq!(invocation.argv[3], "--hyperframes");
    assert!(invocation.argv[4].ends_with("engines/node_modules/hyperframes"));
    assert_eq!(invocation.argv[5..], ["-o", "/tmp/out.mp4"]);
}
