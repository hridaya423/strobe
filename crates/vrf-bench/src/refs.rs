use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, ensure};

use crate::corpus::{Corpus, Framework, bench_dir};
use crate::engines::{headless_shell, node_bin};
use crate::score::{self, Report, Sampling, png_frames};

pub fn build(c: &Corpus, keep: Sampling, out: &Path) -> Result<Report> {
    std::fs::create_dir_all(out)?;
    let (run1, run2) = (out.join("frames"), out.join("run2"));
    render(c, keep, &run1)?;
    render(c, keep, &run2)?;
    let floor = score::score(
        &run1,
        &run2,
        Sampling::All,
        Some(&out.join("noise_floor.worst-flip.png")),
    )?;
    std::fs::write(
        out.join("noise_floor.json"),
        serde_json::to_string_pretty(&floor)?,
    )?;
    std::fs::remove_dir_all(&run2)?;
    Ok(floor)
}

fn render(c: &Corpus, keep: Sampling, dest: &Path) -> Result<()> {
    let raw = std::env::temp_dir().join(format!(
        "vrf-refs-{}-{}",
        c.id,
        dest.file_name().unwrap().to_string_lossy()
    ));
    for d in [dest, raw.as_path()] {
        if d.exists() {
            std::fs::remove_dir_all(d)?;
        }
    }
    let raw_s = raw.display().to_string();
    let (cwd, argv, first): (_, Vec<String>, u32) = match c.framework {
        Framework::Hyperframes => (
            bench_dir().join("engines"),
            vec![
                node_bin(&bench_dir().join("engines"), "hyperframes")
                    .display()
                    .to_string(),
                "render".into(),
                c.project_dir().display().to_string(),
                "--format".into(),
                "png-sequence".into(),
                "-o".into(),
                raw_s,
            ],
            1,
        ),
        Framework::Remotion => (
            c.project_dir(),
            vec![
                node_bin(&c.project_dir(), "remotion").display().to_string(),
                "render".into(),
                c.entry.display().to_string(),
                c.composition.clone().unwrap_or_default(),
                raw_s,
                "--sequence".into(),
                "--image-format".into(),
                "png".into(),
                "--browser-executable".into(),
                headless_shell().display().to_string(),
            ],
            0,
        ),
    };
    let log = dest.with_extension("log");
    let output = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(&cwd)
        .env("HYPERFRAMES_NO_TELEMETRY", "1")
        .env("DO_NOT_TRACK", "1")
        .env("REMOTION_DISABLE_TELEMETRY", "1")
        .output()
        .with_context(|| format!("running {}", argv[0]))?;
    std::fs::write(
        &log,
        [output.stdout.as_slice(), output.stderr.as_slice()].concat(),
    )?;
    ensure!(
        output.status.success(),
        "reference render of {} failed; see {}",
        c.id,
        log.display()
    );

    let frames = png_frames(&raw)?;
    ensure!(
        frames.len() == c.frames as usize && frames[0].0 == first,
        "{} rendered {} frames starting at {}, expected {} starting at {first}",
        c.id,
        frames.len(),
        frames[0].0,
        c.frames
    );
    std::fs::create_dir_all(dest)?;
    for (n, path) in frames {
        let index = n - first;
        if keep.keeps(index) {
            std::fs::rename(&path, dest.join(format!("{index:06}.png")))?;
        }
    }
    std::fs::remove_dir_all(&raw)?;
    Ok(())
}
