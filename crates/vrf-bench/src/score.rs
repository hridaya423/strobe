use std::io::Read;
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};

use anyhow::{Context, Result, ensure};
use serde::Serialize;

#[derive(Clone)]
pub struct Rgb8 {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Rgb8 {
    pub fn load_png(path: &Path) -> Result<Rgb8> {
        let img = image::open(path)
            .with_context(|| format!("decoding {}", path.display()))?
            .into_rgb8();
        Ok(Rgb8 {
            width: img.width(),
            height: img.height(),
            data: img.into_raw(),
        })
    }

    pub fn save_png(&self, path: &Path) -> Result<()> {
        image::save_buffer(
            path,
            &self.data,
            self.width,
            self.height,
            image::ColorType::Rgb8,
        )
        .with_context(|| format!("writing {}", path.display()))
    }

    fn pixels(&self) -> usize {
        (self.width * self.height) as usize
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FrameScore {
    pub flip_mean: f64,
    pub flip_p999: f64,
    pub max_delta: u8,
    pub p999_delta: f64,
    pub frac_over_8: f64,
    pub frac_over_32: f64,
    pub ssim: f64,
}

pub fn compare(a: &Rgb8, b: &Rgb8) -> Result<FrameScore> {
    Ok(compare_with_map(a, b)?.0)
}

pub fn compare_with_map(a: &Rgb8, b: &Rgb8) -> Result<(FrameScore, nv_flip::FlipImageFloat)> {
    ensure!(
        a.width == b.width && a.height == b.height,
        "frame sizes differ: {}x{} vs {}x{}",
        a.width,
        a.height,
        b.width,
        b.height
    );
    let mut hist = [0u64; 256];
    for (pa, pb) in a.data.chunks_exact(3).zip(b.data.chunks_exact(3)) {
        let d = pa
            .iter()
            .zip(pb)
            .map(|(x, y)| x.abs_diff(*y))
            .max()
            .unwrap_or(0);
        hist[d as usize] += 1;
    }
    let n = a.pixels() as f64;
    let over = |t: usize| hist[t + 1..].iter().sum::<u64>() as f64 / n;

    let map = nv_flip::flip(
        nv_flip::FlipImageRgb8::with_data(a.width, a.height, &a.data),
        nv_flip::FlipImageRgb8::with_data(b.width, b.height, &b.data),
        nv_flip::DEFAULT_PIXELS_PER_DEGREE,
    );
    let mut errors = map.to_vec();
    let flip_mean = errors.iter().map(|&e| e as f64).sum::<f64>() / n;
    errors.sort_unstable_by(f32::total_cmp);

    let score = FrameScore {
        flip_mean,
        flip_p999: percentile_sorted(&errors, 0.999),
        max_delta: hist.iter().rposition(|&c| c > 0).unwrap_or(0) as u8,
        p999_delta: percentile_hist(&hist, 0.999),
        frac_over_8: over(8),
        frac_over_32: over(32),
        ssim: ssim(a, b),
    };
    Ok((score, map))
}

pub fn heatmap(map: &nv_flip::FlipImageFloat) -> Rgb8 {
    let rgb = map.apply_color_lut(&nv_flip::magma_lut());
    Rgb8 {
        width: rgb.width(),
        height: rgb.height(),
        data: rgb.to_vec(),
    }
}

fn percentile_sorted(sorted: &[f32], q: f64) -> f64 {
    let rank = q * (sorted.len() - 1) as f64;
    let (lo, hi) = (rank.floor() as usize, rank.ceil() as usize);
    let (vlo, vhi) = (sorted[lo] as f64, sorted[hi] as f64);
    vlo + (vhi - vlo) * (rank - lo as f64)
}

fn percentile_hist(hist: &[u64; 256], q: f64) -> f64 {
    let total: u64 = hist.iter().sum();
    let rank = q * (total - 1) as f64;
    let value_at = |r: u64| {
        let mut seen = 0;
        hist.iter()
            .position(|&c| {
                seen += c;
                seen > r
            })
            .unwrap_or(255) as f64
    };
    let (lo, hi) = (rank.floor() as u64, rank.ceil() as u64);
    let (vlo, vhi) = (value_at(lo), value_at(hi));
    vlo + (vhi - vlo) * (rank - lo as f64)
}

const WIN: usize = 7;

fn ssim(a: &Rgb8, b: &Rgb8) -> f64 {
    (0..3).map(|c| ssim_channel(a, b, c)).sum::<f64>() / 3.0
}

fn ssim_channel(a: &Rgb8, b: &Rgb8, channel: usize) -> f64 {
    let (w, h) = (a.width as usize, a.height as usize);
    let stride = w + 1;
    let mut sums = vec![[0f64; 5]; stride * (h + 1)];
    for y in 0..h {
        let mut row = [0f64; 5];
        for x in 0..w {
            let i = (y * w + x) * 3 + channel;
            let (p, q) = (a.data[i] as f64 / 255.0, b.data[i] as f64 / 255.0);
            for (r, v) in row.iter_mut().zip([p, q, p * p, q * q, p * q]) {
                *r += v;
            }
            let above = sums[y * stride + x + 1];
            let cell = &mut sums[(y + 1) * stride + x + 1];
            for k in 0..5 {
                cell[k] = above[k] + row[k];
            }
        }
    }
    let np = (WIN * WIN) as f64;
    let cov_norm = np / (np - 1.0);
    let (c1, c2) = (0.01f64.powi(2), 0.03f64.powi(2));
    let pad = WIN / 2;
    let mut total = 0.0;
    for y in pad..h - pad {
        for x in pad..w - pad {
            let (y0, x0, y1, x1) = (y - pad, x - pad, y + pad + 1, x + pad + 1);
            let mut m = [0f64; 5];
            for (k, v) in m.iter_mut().enumerate() {
                *v = (sums[y1 * stride + x1][k]
                    - sums[y0 * stride + x1][k]
                    - sums[y1 * stride + x0][k]
                    + sums[y0 * stride + x0][k])
                    / np;
            }
            let [ux, uy, uxx, uyy, uxy] = m;
            let vx = cov_norm * (uxx - ux * ux);
            let vy = cov_norm * (uyy - uy * uy);
            let vxy = cov_norm * (uxy - ux * uy);
            total += ((2.0 * ux * uy + c1) * (2.0 * vxy + c2))
                / ((ux * ux + uy * uy + c1) * (vx + vy + c2));
        }
    }
    total / ((h - 2 * pad) * (w - 2 * pad)) as f64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sampling {
    All,
    Every(u32),
}

impl std::str::FromStr for Sampling {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Sampling> {
        match s {
            "all" => Ok(Sampling::All),
            _ => {
                let n = s
                    .strip_prefix("every:")
                    .context("expected all or every:N")?
                    .parse()?;
                ensure!(n > 0, "every:N needs N of at least 1");
                Ok(Sampling::Every(n))
            }
        }
    }
}

impl Sampling {
    pub fn keeps(self, index: u32) -> bool {
        match self {
            Sampling::All => true,
            Sampling::Every(n) => index % n == 0,
        }
    }
}

pub type Frames = Box<dyn Iterator<Item = Result<(u32, Rgb8)>> + Send>;

pub struct Source {
    pub frames: Frames,
    pub sparse: bool,
}

pub fn frame_index(path: &Path) -> Option<u32> {
    let stem = path.file_stem()?.to_str()?;
    let end = stem.rfind(|c: char| c.is_ascii_digit())? + 1;
    let start = stem[..end]
        .rfind(|c: char| !c.is_ascii_digit())
        .map_or(0, |i| i + 1);
    stem[start..end].parse().ok()
}

pub fn png_frames(dir: &Path) -> Result<Vec<(u32, std::path::PathBuf)>> {
    let mut pngs: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("png")))
        .map(|p| {
            frame_index(&p)
                .map(|i| (i, p.clone()))
                .with_context(|| format!("no frame number in {}", p.display()))
        })
        .collect::<Result<_>>()?;
    pngs.sort();
    ensure!(!pngs.is_empty(), "no PNG frames in {}", dir.display());
    Ok(pngs)
}

pub fn open(path: &Path) -> Result<Source> {
    if path.is_dir() {
        let pngs = png_frames(path)?;
        let frames = pngs.into_iter().map(|(i, p)| Ok((i, Rgb8::load_png(&p)?)));
        return Ok(Source {
            frames: Box::new(frames),
            sparse: true,
        });
    }
    Ok(Source {
        frames: Box::new(decode(path)?.zip(0u32..).map(|(f, i)| f.map(|f| (i, f)))),
        sparse: false,
    })
}

pub fn parse_size(csv: &str) -> Result<(u32, u32)> {
    let mut fields = csv.trim().split(',');
    let mut next = || fields.next().context("ffprobe printed no size");
    Ok((next()?.parse()?, next()?.parse()?))
}

fn decode(path: &Path) -> Result<Decoder> {
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .context("running ffprobe")?;
    ensure!(
        probe.status.success(),
        "ffprobe failed on {}",
        path.display()
    );
    let dims = String::from_utf8(probe.stdout)?;
    let (width, height) = parse_size(&dims)?;
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-vf",
            "scale=in_color_matrix=auto:in_range=auto:flags=accurate_rnd+full_chroma_int+bitexact,format=rgb24",
            "-f",
            "rawvideo",
            "-",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .context("running ffmpeg")?;
    let out = child.stdout.take().context("ffmpeg stdout")?;
    Ok(Decoder {
        child,
        out,
        width,
        height,
    })
}

struct Decoder {
    child: Child,
    out: ChildStdout,
    width: u32,
    height: u32,
}

impl Iterator for Decoder {
    type Item = Result<Rgb8>;

    fn next(&mut self) -> Option<Result<Rgb8>> {
        let mut data = vec![0; (self.width * self.height * 3) as usize];
        match self.out.read_exact(&mut data) {
            Ok(()) => Some(Ok(Rgb8 {
                width: self.width,
                height: self.height,
                data,
            })),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => match self.child.wait() {
                Ok(s) if s.success() => None,
                Ok(s) => Some(Err(anyhow::anyhow!("ffmpeg exited with {s}"))),
                Err(e) => Some(Err(e.into())),
            },
            Err(e) => Some(Err(e.into())),
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Extreme {
    pub frame: u32,
    pub value: f64,
}

#[derive(Debug, Serialize)]
pub struct Worst {
    pub flip_mean: Extreme,
    pub flip_p999: Extreme,
    pub max_delta: Extreme,
    pub frac_over_8: Extreme,
    pub frac_over_32: Extreme,
    pub ssim: Extreme,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub a: String,
    pub b: String,
    pub frames_compared: usize,
    pub worst: Worst,
    pub per_frame: Vec<(u32, FrameScore)>,
}

pub fn score(
    a: &Path,
    b: &Path,
    sampling: Sampling,
    heatmap_path: Option<&Path>,
) -> Result<Report> {
    let (mut sa, mut sb) = (open(a)?, open(b)?);
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get());
    let (job_tx, job_rx) = std::sync::mpsc::sync_channel::<(u32, Rgb8, Rgb8)>(workers * 2);
    let job_rx = std::sync::Mutex::new(job_rx);
    let mut per_frame = std::thread::scope(|scope| -> Result<Vec<(u32, FrameScore)>> {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| -> Result<Vec<(u32, FrameScore)>> {
                    let mut done = Vec::new();
                    loop {
                        let job = job_rx.lock().unwrap().recv();
                        let Ok((i, fa, fb)) = job else {
                            return Ok(done);
                        };
                        done.push((i, compare(&fa, &fb).with_context(|| format!("frame {i}"))?));
                    }
                })
            })
            .collect();
        let fed = join(&mut sa, &mut sb, a, b, |i, fa, fb| {
            sampling.keeps(i) && job_tx.send((i, fa, fb)).is_err()
        });
        drop(job_tx);
        let mut all = Vec::new();
        for h in handles {
            all.extend(h.join().expect("score worker panicked")?);
        }
        fed.map(|()| all)
    })?;
    ensure!(!per_frame.is_empty(), "no frames compared");
    per_frame.sort_by_key(|(i, _)| *i);

    let pick = |key: fn(&FrameScore) -> f64, highest: bool| {
        let (frame, s) = per_frame
            .iter()
            .max_by(|(_, x), (_, y)| {
                let o = key(x).total_cmp(&key(y));
                if highest { o } else { o.reverse() }
            })
            .unwrap();
        Extreme {
            frame: *frame,
            value: key(s),
        }
    };
    let worst = Worst {
        flip_mean: pick(|s| s.flip_mean, true),
        flip_p999: pick(|s| s.flip_p999, true),
        max_delta: pick(|s| s.max_delta as f64, true),
        frac_over_8: pick(|s| s.frac_over_8, true),
        frac_over_32: pick(|s| s.frac_over_32, true),
        ssim: pick(|s| s.ssim, false),
    };
    if let Some(path) = heatmap_path {
        let n = worst.flip_mean.frame;
        let find = |p: &Path| -> Result<Rgb8> {
            for f in open(p)?.frames {
                let (i, f) = f?;
                if i == n {
                    return Ok(f);
                }
            }
            anyhow::bail!("frame {n} vanished from {}", p.display())
        };
        let (fa, fb) = (find(a)?, find(b)?);
        heatmap(&compare_with_map(&fa, &fb)?.1).save_png(path)?;
    }
    Ok(Report {
        a: a.display().to_string(),
        b: b.display().to_string(),
        frames_compared: per_frame.len(),
        worst,
        per_frame,
    })
}

fn join(
    sa: &mut Source,
    sb: &mut Source,
    a: &Path,
    b: &Path,
    mut pair: impl FnMut(u32, Rgb8, Rgb8) -> bool,
) -> Result<()> {
    use std::cmp::Ordering;
    let (mut x, mut y) = (sa.frames.next().transpose()?, sb.frames.next().transpose()?);
    let mut compared = 0;
    loop {
        let order = match (&x, &y) {
            (None, None) => return Ok(()),
            (Some((i, _)), Some((j, _))) => i.cmp(j),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
        };
        match order {
            Ordering::Equal => {
                let ((i, fa), (_, fb)) = (x.take().unwrap(), y.take().unwrap());
                if pair(i, fa, fb) {
                    return Ok(());
                }
                compared = i + 1;
                x = sa.frames.next().transpose()?;
                y = sb.frames.next().transpose()?;
            }
            Ordering::Less => {
                missing(sb.sparse, b, x.as_ref().unwrap().0, y.is_none(), compared)?;
                x = sa.frames.next().transpose()?;
            }
            Ordering::Greater => {
                missing(sa.sparse, a, y.as_ref().unwrap().0, x.is_none(), compared)?;
                y = sb.frames.next().transpose()?;
            }
        }
    }
}

fn missing(sparse: bool, path: &Path, frame: u32, ended: bool, frames: u32) -> Result<()> {
    ensure!(
        sparse || !ended,
        "{} has only {frames} frames",
        path.display()
    );
    ensure!(sparse, "{} has no frame {frame}", path.display());
    Ok(())
}
