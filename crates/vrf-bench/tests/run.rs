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
