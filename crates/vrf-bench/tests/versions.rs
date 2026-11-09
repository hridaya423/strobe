use vrf_bench::corpus::bench_dir;
use vrf_bench::engines::{HEADLESS_SHELL, HYPERFRAMES, REMOTION};

fn read(relative: &str) -> String {
    let path = bench_dir().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn four_part_versions(text: &str) -> Vec<&str> {
    text.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .filter(|token| {
            let parts: Vec<_> = token.split('.').collect();
            parts.len() == 4 && parts.iter().all(|p| !p.is_empty())
        })
        .collect()
}

#[test]
fn scripts_pin_the_headless_shell_the_harness_runs() {
    for script in ["gate-b.sh", "fetch-external.sh"] {
        let text = read(script);
        let versions = four_part_versions(&text);
        assert!(!versions.is_empty(), "{script} names no headless shell");
        for version in versions {
            assert_eq!(version, HEADLESS_SHELL, "{script}");
        }
    }
}

#[test]
fn package_json_pins_the_versions_the_harness_records() {
    let package: serde_json::Value = serde_json::from_str(&read("engines/package.json")).unwrap();
    let pinned = |name: &str| package["dependencies"][name].as_str().unwrap().to_string();
    assert_eq!(pinned("hyperframes"), HYPERFRAMES);
    for name in [
        "remotion",
        "@remotion/cli",
        "@remotion/bundler",
        "@remotion/renderer",
    ] {
        assert_eq!(pinned(name), REMOTION, "{name}");
    }
}
