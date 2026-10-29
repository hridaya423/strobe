use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use anyhow::{Result, ensure};
use vrf_blink::video::Frame;

const GENERATOR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../spikes/5.1-video-into-page/gen.sh"
);

static GENERATING: Mutex<()> = Mutex::new(());

pub fn clip(name: &str) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join("vrf-video-clips");
    let path = dir.join(name);
    let _guard = GENERATING.lock().unwrap();
    if !path.exists() {
        let partial = dir.join(format!("partial-{}", std::process::id()));
        let status = Command::new("sh")
            .arg(GENERATOR)
            .arg(&partial)
            .arg(name)
            .output()?;
        ensure!(status.status.success(), "cannot generate {name}");
        std::fs::create_dir_all(&dir)?;
        std::fs::rename(partial.join(name), &path)?;
        std::fs::remove_dir_all(&partial)?;
    }
    Ok(path)
}

pub fn frame_id(frame: &Frame, width: u32, height: u32) -> u32 {
    let mut planes = vec![0; Frame::nv12_len(width, height)];
    frame.write_nv12(&mut planes);
    let cell = width as usize / 48;
    (0..24)
        .filter(|bit| planes[cell / 2 * width as usize + bit * cell + cell / 2] > 126)
        .fold(0, |id, bit| id | 1 << bit)
}

pub fn packet_pts(path: &Path) -> Result<Vec<i64>> {
    let output = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "packet=pts", "-of", "csv=p=0"])
        .arg(path)
        .output()?;
    ensure!(output.status.success(), "ffprobe failed");
    let mut pts: Vec<i64> = String::from_utf8(output.stdout)?
        .lines()
        .filter_map(|line| line.trim_end_matches(',').parse().ok())
        .collect();
    pts.sort();
    Ok(pts)
}
