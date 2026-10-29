use vrf_bench::corpus::select;

fn ids(spec: &str) -> Vec<String> {
    select(spec).unwrap().into_iter().map(|c| c.id).collect()
}

#[test]
fn groups_split_the_launch_videos_from_the_synthetic_set() {
    let all = ids("all");
    let synth = ids("synth");
    let launch = ids("launch");
    assert!(!synth.is_empty());
    assert!(synth.iter().all(|id| id.starts_with("synth-")));
    assert!(launch.iter().all(|id| !id.starts_with("synth-")));
    assert!(launch.contains(&"rm-transitions-video".to_string()));
    assert_eq!(all.len(), synth.len() + launch.len());
}

#[test]
fn every_synthetic_axis_exists_for_both_frameworks() {
    let synth = ids("synth");
    for id in &synth {
        let stem = id.rsplit_once('-').unwrap().0;
        for suffix in ["hf", "rm"] {
            let twin = format!("{stem}-{suffix}");
            assert!(synth.contains(&twin), "{twin} is missing");
        }
    }
}

#[test]
fn synthetic_projects_exist_on_disk() {
    for c in select("synth").unwrap() {
        assert!(c.entry_path().is_file(), "{} entry missing", c.id);
    }
}
