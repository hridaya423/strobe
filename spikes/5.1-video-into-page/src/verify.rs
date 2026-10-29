use std::io::Read;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, ensure};

pub const BITS: usize = 24;
pub const PATCHES: usize = 8;
const BOX: i64 = 8;

pub struct Layout {
    pub width: usize,
    pub height: usize,
}

impl Layout {
    fn cell(&self) -> usize {
        self.width / 48
    }

    fn bit_centre(&self, bit: usize) -> (usize, usize) {
        (bit * self.cell() + self.cell() / 2, self.cell() / 2)
    }

    pub fn patch_centre(&self, patch: usize) -> (usize, usize) {
        (
            patch * self.width / 8 + self.width / 16,
            self.height / 3 + self.height / 12,
        )
    }
}

#[derive(Clone, Debug, Default)]
pub struct Reading {
    pub id: u64,
    pub patches: [[f64; 3]; PATCHES],
}

pub fn read(layout: &Layout, pixel: impl Fn(usize, usize) -> [u8; 3]) -> Reading {
    let mut id = 0u64;
    for bit in 0..BITS {
        let (x, y) = layout.bit_centre(bit);
        let [r, g, b] = pixel(x, y);
        if (u32::from(r) + u32::from(g) + u32::from(b)) / 3 > 128 {
            id |= 1 << bit;
        }
    }
    let mut patches = [[0.0; 3]; PATCHES];
    for (index, patch) in patches.iter_mut().enumerate() {
        let (cx, cy) = layout.patch_centre(index);
        let mut sum = [0.0; 3];
        for dy in -BOX / 2..BOX / 2 {
            for dx in -BOX / 2..BOX / 2 {
                let rgb = pixel((cx as i64 + dx) as usize, (cy as i64 + dy) as usize);
                for c in 0..3 {
                    sum[c] += f64::from(rgb[c]);
                }
            }
        }
        for c in 0..3 {
            patch[c] = sum[c] / (BOX * BOX) as f64;
        }
    }
    Reading { id, patches }
}

pub fn max_patch_error(a: &Reading, b: &Reading) -> f64 {
    let mut worst: f64 = 0.0;
    for p in 0..PATCHES {
        for c in 0..3 {
            worst = worst.max((a.patches[p][c] - b.patches[p][c]).abs());
        }
    }
    worst
}

pub struct Probe {
    pub pts: Vec<i64>,
    pub tick_num: i64,
    pub tick_den: i64,
    pub width: usize,
    pub height: usize,
}

pub fn probe(path: &str) -> Result<Probe> {
    let output = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=time_base,width,height:frame=pts", "-of", "json", path])
        .output()?;
    ensure!(output.status.success(), "ffprobe failed");
    let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let (num, den) = json["streams"][0]["time_base"]
        .as_str()
        .and_then(|text| text.split_once('/'))
        .context("no time base")?;
    let mut pts: Vec<i64> = json["frames"]
        .as_array()
        .context("no frames")?
        .iter()
        .filter_map(|frame| frame["pts"].as_i64())
        .collect();
    pts.sort_unstable();
    Ok(Probe {
        pts,
        tick_num: num.parse()?,
        tick_den: den.parse()?,
        width: json["streams"][0]["width"].as_u64().context("no width")? as usize,
        height: json["streams"][0]["height"].as_u64().context("no height")? as usize,
    })
}

impl Probe {
    pub fn seconds(&self, pts: i64) -> f64 {
        pts as f64 * self.tick_num as f64 / self.tick_den as f64
    }

    pub fn output_frames(&self, fps: i64) -> usize {
        let last = self.seconds(*self.pts.last().unwrap());
        (last * fps as f64).ceil() as usize + 1
    }
}

/// The latest frame whose pts is at or before `target`, or the earliest frame if none is.
pub fn choose(pts: &[i64], target: i64) -> usize {
    pts.partition_point(|&p| p <= target).saturating_sub(1)
}

pub fn target_ticks(frame: usize, fps: i64, timescale: i64) -> i64 {
    (frame as f64 * timescale as f64 / fps as f64).round() as i64
}

pub fn expected(path: &str, matrix: &str, range: &str, layout: &Layout) -> Result<Vec<Reading>> {
    let filter = format!(
        "scale=in_color_matrix={matrix}:in_range={range}:out_range=full:flags=accurate_rnd+full_chroma_int,format=rgb24"
    );
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-i", path, "-vf", &filter, "-fps_mode", "passthrough", "-f", "rawvideo", "-"])
        .stdout(Stdio::piped())
        .spawn()?;
    let mut stdout = child.stdout.take().unwrap();
    let mut frame = vec![0u8; layout.width * layout.height * 3];
    let mut readings = Vec::new();
    while stdout.read_exact(&mut frame).is_ok() {
        readings.push(read(layout, |x, y| {
            let i = (y * layout.width + x) * 3;
            [frame[i], frame[i + 1], frame[i + 2]]
        }));
    }
    ensure!(child.wait()?.success(), "ffmpeg failed");
    Ok(readings)
}
