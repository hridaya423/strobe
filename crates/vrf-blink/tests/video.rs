mod clips;
#[allow(dead_code)]
mod support;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use clips::{clip, packet_pts};
use serde_json::json;
use vrf::gpu::Gpu;
use vrf_blink::video::{Clips, Link};
use vrf_blink::{Mode, Project, Ui, View};

const SIZE: (i32, i32) = (1920, 1080);

struct Stage {
    gpu: Gpu,
    view: View,
}

fn stage(ui: &Ui, names: &[&str]) -> Result<Stage> {
    let dir = std::env::temp_dir().join(format!("vrf-video-page-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir)?;
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fs::copy(here.join("tests/video/index.html"), dir.join("index.html"))?;
    fs::copy(here.join("js/video.js"), dir.join("video.js"))?;
    for name in names {
        symlink(clip(name)?, dir.join(name))?;
    }
    let project = Project::open(&dir, Mode::Offline)?;
    let mut view = View::open_project(ui, &project, "index.html", SIZE.0, SIZE.1, None)?;
    Link::attach(
        &mut view,
        project,
        Arc::new(Clips::default()),
        Arc::new(|| anyhow::bail!("no fallback in this test")),
    );
    Ok(Stage {
        gpu: Gpu::new()?,
        view,
    })
}

impl Stage {
    fn show(&mut self, items: serde_json::Value) -> Result<Vec<u8>> {
        self.view.evaluate(&format!("show({items})"))?;
        self.view.begin_frame();
        let paint = self
            .view
            .recv_paint(Duration::from_secs(10))
            .context("no delivery")?;
        let texture = self.gpu.import_iosurface(&paint.surface, 0, paint.format)?;
        Ok(self.gpu.read_texture(&texture)?)
    }
}

fn pixel(bgra: &[u8], x: usize, y: usize) -> [u8; 3] {
    let at = (y * SIZE.0 as usize + x) * 4;
    [bgra[at + 2], bgra[at + 1], bgra[at]]
}

fn frame_id(bgra: &[u8]) -> u32 {
    let cell = SIZE.0 as usize / 48;
    (0..24)
        .filter(|bit| pixel(bgra, bit * cell + cell / 2, cell / 2)[0] > 128)
        .fold(0, |id, bit| id | 1 << bit)
}

fn expected(pts: &[i64], timescale: i64, time: f64) -> usize {
    let target = (time * timescale as f64).trunc() as i64;
    let after = pts.partition_point(|p| *p <= target);
    match after {
        0 => 0,
        n if n == pts.len() => n - 1,
        n => {
            let (before, next) = (pts[n - 1], pts[n]);
            if target - before < next - target {
                n - 1
            } else {
                n
            }
        }
    }
}

fn ids_are_exact(stage: &mut Stage, name: &str, order: impl Fn(usize) -> usize) -> Result<()> {
    let pts = packet_pts(&clip(name)?)?;
    let timescale = if name.starts_with("vfr") { 1000 } else { 15360 };
    let last = *pts.last().unwrap() as f64 / timescale as f64;
    let steps = ((last + 0.5) * 30.0) as usize;
    for step in 0..steps {
        let time = order(step) as f64 / 30.0;
        let image = stage.show(json!([[
            format!("vrf://project/{name}"),
            time,
            "position:absolute;left:0;top:0"
        ]]))?;
        let want = expected(&pts, timescale, time);
        let got = frame_id(&image);
        ensure!(
            got as usize == want,
            "{name}: time {time:.4} shows frame {got}, expected {want}"
        );
    }
    Ok(())
}

fn ids_are_exact_in_order_on_cfr_vfr_offset_and_hevc(ui: &Ui) -> Result<()> {
    for name in [
        "cfr1080.mp4",
        "vfr1080.mp4",
        "off1080.mp4",
        "cfr1080-hevc.mp4",
    ] {
        let mut stage = stage(ui, &[name])?;
        ids_are_exact(&mut stage, name, |step| step)?;
        stage.view.close()?;
    }
    Ok(())
}

fn ids_are_exact_when_seeking_backwards_and_jumping(ui: &Ui) -> Result<()> {
    for name in ["cfr1080.mp4", "vfr1080.mp4", "off1080.mp4"] {
        let mut stage = stage(ui, &[name])?;
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut at = 0usize;
        let order: Vec<usize> = (0..200)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                at = match seed % 8 {
                    0..=3 => at + 1,
                    4 => at.saturating_sub(1 + (seed >> 8) as usize % 6),
                    5 => (seed >> 16) as usize % 190,
                    6 => at,
                    _ => at + 2 + (seed >> 8) as usize % 4,
                } % 190;
                at
            })
            .collect();
        ids_are_exact(&mut stage, name, |step| order[step % order.len()])?;
        stage.view.close()?;
    }
    Ok(())
}

const CASES: &[support::Case] = &[
    (
        "ids_are_exact_in_order_on_cfr_vfr_offset_and_hevc",
        ids_are_exact_in_order_on_cfr_vfr_offset_and_hevc,
    ),
    (
        "ids_are_exact_when_seeking_backwards_and_jumping",
        ids_are_exact_when_seeking_backwards_and_jumping,
    ),
];

fn main() -> ExitCode {
    support::run(CASES)
}
