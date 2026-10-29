#[allow(dead_code)]
mod support;

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use vrf_blink::driver::{Config, render};
use vrf_blink::{Mode, Project, Ui, View};
use vrf_encode::{Codec, Rational};

const SCRIPT: &str = "window.cdnLoaded = \"pinned\";\n";

struct Cdn {
    address: SocketAddr,
    hits: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Cdn {
    fn start() -> Result<Cdn> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let hits = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (hits, stop) = (hits.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    let Ok((mut stream, _)) = listener.accept() else {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    };
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let mut request = Vec::new();
                    let mut chunk = [0u8; 1024];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        match stream.read(&mut chunk) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => request.extend_from_slice(&chunk[..n]),
                        }
                    }
                    hits.fetch_add(1, Ordering::AcqRel);
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{SCRIPT}",
                        SCRIPT.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                }
            })
        };
        Ok(Cdn {
            address,
            hits,
            stop,
            thread: Some(thread),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::Acquire)
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

impl Drop for Cdn {
    fn drop(&mut self) {
        self.stop();
    }
}

fn scratch_root() -> PathBuf {
    std::env::temp_dir().join(format!("vrf-blink-project-{}", std::process::id()))
}

fn scratch(name: &str, cdn: &str) -> Result<PathBuf> {
    let dir = scratch_root().join(name);
    fs::create_dir_all(&dir)?;
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/project");
    for entry in fs::read_dir(source)? {
        let path = entry?.path();
        let target = dir.join(path.file_name().unwrap());
        match path.extension().and_then(|e| e.to_str()) {
            Some("html") => fs::write(&target, fs::read_to_string(&path)?.replace("{{CDN}}", cdn))?,
            _ => {
                fs::copy(&path, &target)?;
            }
        }
    }
    Ok(dir)
}

fn sha256(path: &Path) -> Result<String> {
    let output = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()?;
    ensure!(output.status.success(), "shasum failed");
    Ok(String::from_utf8(output.stdout)?
        .split_whitespace()
        .next()
        .context("no digest")?
        .to_owned())
}

fn lock(dir: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(
        dir.join("vrf.lock.json"),
    )?)?)
}

const ASSETS: &str = r#"(async () => {
  await document.fonts.load('100px VrfBox');
  const image = document.getElementById('image');
  await image.decode();
  const canvas = document.createElement('canvas');
  canvas.width = canvas.height = 1;
  const context = canvas.getContext('2d');
  context.drawImage(image, 0, 0);
  return {
    cdn: window.cdnLoaded ?? null,
    glyph: document.getElementById('glyph').getBoundingClientRect().width,
    pixel: Array.from(context.getImageData(0, 0, 1, 1).data),
    size: [image.naturalWidth, image.naturalHeight],
  };
})()"#;

fn check_assets(view: &mut View) -> Result<()> {
    let state = view.evaluate(ASSETS)?;
    ensure!(
        state
            == json!({"cdn": "pinned", "glyph": 100, "pixel": [255, 0, 0, 255], "size": [16, 16]}),
        "page state: {state}"
    );
    Ok(())
}

fn pin_online(ui: &Ui, dir: &Path) -> Result<()> {
    let project = Project::open(dir, Mode::Online)?;
    let mut view = View::open_project(ui, &project, "index.html", 320, 240, None)?;
    check_assets(&mut view)?;
    view.close()
}

fn online_pins_and_renders(ui: &Ui) -> Result<()> {
    let cdn = Cdn::start()?;
    let url = cdn.url("/lib.js");
    let dir = scratch("online", &url)?;
    pin_online(ui, &dir)?;
    ensure!(
        cdn.hits() == 1,
        "the CDN saw {} requests, expected 1",
        cdn.hits()
    );

    let lock = lock(&dir)?;
    let pins = lock["pins"].as_array().context("no pins array")?;
    ensure!(pins.len() == 1, "lock: {lock}");
    let cache = dir.join(pins[0]["path"].as_str().context("pin has no path")?);
    ensure!(pins[0]["url"] == url, "lock: {lock}");
    ensure!(
        fs::read_to_string(&cache)? == SCRIPT,
        "cache holds other bytes"
    );
    ensure!(
        pins[0]["sha256"] == sha256(&cache)?.as_str(),
        "lock digest {} differs from the cached file",
        pins[0]["sha256"]
    );
    Ok(())
}

fn offline_serves_pinned_without_network(ui: &Ui) -> Result<()> {
    let mut cdn = Cdn::start()?;
    let dir = scratch("offline", &cdn.url("/lib.js"))?;
    pin_online(ui, &dir)?;
    ensure!(cdn.hits() == 1, "pinning made {} requests", cdn.hits());

    let project = Project::open(&dir, Mode::Offline)?;
    let mut view = View::open_project(ui, &project, "index.html", 320, 240, None)?;
    check_assets(&mut view)?;
    view.close()?;
    ensure!(
        cdn.hits() == 1,
        "an offline load reached the network ({} requests)",
        cdn.hits()
    );

    cdn.stop();
    let project = Project::open(&dir, Mode::Offline)?;
    let mut view = View::open_project(ui, &project, "index.html", 320, 240, None)?;
    check_assets(&mut view)?;
    view.close()
}

fn offline_serves_https_pin_from_lock(ui: &Ui) -> Result<()> {
    let url = "https://cdn.invalid/lib.js";
    let dir = scratch("https", url)?;
    let cache = dir.join(".vrf/cache");
    fs::create_dir_all(&cache)?;
    let staged = cache.join("staged");
    fs::write(&staged, SCRIPT)?;
    let sha256 = sha256(&staged)?;
    fs::rename(&staged, cache.join(&sha256))?;
    fs::write(
        dir.join("vrf.lock.json"),
        json!({
            "version": 1,
            "pins": [{
                "url": url,
                "sha256": sha256,
                "path": format!(".vrf/cache/{sha256}"),
                "content_type": "text/javascript; charset=utf-8",
            }],
        })
        .to_string(),
    )?;

    let project = Project::open(&dir, Mode::Offline)?;
    let mut view = View::open_project(ui, &project, "index.html", 320, 240, None)?;
    check_assets(&mut view)?;
    view.close()
}

fn offline_rejects_unpinned(ui: &Ui) -> Result<()> {
    let cdn = Cdn::start()?;
    let url = cdn.url("/lib.js");
    let dir = scratch("unpinned", &url)?;
    let project = Project::open(&dir, Mode::Offline)?;
    let Err(error) = View::open_project(ui, &project, "index.html", 320, 240, None) else {
        bail!("an unpinned dependency loaded offline");
    };
    let message = format!("{error:#}");
    ensure!(
        message.contains(&url) && message.contains("not pinned"),
        "unclear failure: {message}"
    );
    ensure!(cdn.hits() == 0, "offline mode sent {} requests", cdn.hits());
    ensure!(
        !dir.join("vrf.lock.json").exists(),
        "offline mode wrote a lock"
    );
    Ok(())
}

fn offline_rejects_tampered_cache(ui: &Ui) -> Result<()> {
    let cdn = Cdn::start()?;
    let dir = scratch("tampered", &cdn.url("/lib.js"))?;
    pin_online(ui, &dir)?;
    let lock = lock(&dir)?;
    let cache = dir.join(lock["pins"][0]["path"].as_str().context("no path")?);
    fs::write(&cache, "window.cdnLoaded = \"tampered\";\n")?;

    let project = Project::open(&dir, Mode::Offline)?;
    let Err(error) = View::open_project(ui, &project, "index.html", 320, 240, None) else {
        bail!("a cache that no longer matches its digest was served");
    };
    let message = format!("{error:#}");
    ensure!(message.contains("sha256"), "unclear failure: {message}");
    Ok(())
}

fn origin_behaves_like_https(ui: &Ui) -> Result<()> {
    let dir = scratch("features", "http://unused.invalid/lib.js")?;
    let project = Project::open(&dir, Mode::Offline)?;
    let mut view = View::open_project(ui, &project, "features.html", 320, 240, None)?;
    let state = view.evaluate(
        r#"(async () => {
          const data = await (await fetch('data.json')).json();
          const module = await import('./module.js');
          const worker = new Worker('worker.js');
          const doubled = await new Promise((resolve, reject) => {
            worker.onmessage = (event) => resolve(event.data);
            worker.onerror = (event) => reject(new Error('worker failed to load'));
            worker.postMessage(21);
          });
          return {
            origin: location.origin,
            secure: isSecureContext,
            frames: data.frames,
            imported: module.answer,
            inline: window.moduleAnswer,
            doubled,
          };
        })()"#,
    )?;
    ensure!(
        state
            == json!({
                "origin": "vrf://project",
                "secure": true,
                "frames": 10,
                "imported": 42,
                "inline": 42,
                "doubled": 42,
            }),
        "page state: {state}"
    );
    view.close()
}

fn range_requests_return_partial_content(ui: &Ui) -> Result<()> {
    let dir = scratch("range", "http://unused.invalid/lib.js")?;
    let clip = fs::read(dir.join("clip.webm"))?;
    let project = Project::open(&dir, Mode::Offline)?;
    let mut view = View::open_project(ui, &project, "features.html", 320, 240, None)?;
    let probe = |view: &mut View, range: Option<&str>, path: &str| -> Result<Value> {
        let headers = range.map_or(json!({}), |r| json!({"Range": r}));
        view.evaluate(&format!(
            r#"(async () => {{
              const response = await fetch({path:?}, {{headers: {headers}}});
              const body = new Uint8Array(await response.arrayBuffer());
              return {{
                status: response.status,
                type: response.headers.get('content-type'),
                range: response.headers.get('content-range'),
                ranges: response.headers.get('accept-ranges'),
                length: response.headers.get('content-length'),
                body: Array.from(body.slice(0, 64)),
                size: body.length,
              }};
            }})()"#
        ))
        .with_context(|| format!("fetching {path} with range {range:?}"))
    };
    let total = clip.len();

    let full = probe(&mut view, None, "clip.webm")?;
    ensure!(
        full["status"] == 200 && full["ranges"] == "bytes",
        "full: {full}"
    );
    ensure!(
        full["type"] == "video/webm" && full["size"] == total,
        "full: {full}"
    );

    let head = probe(&mut view, Some("bytes=0-99"), "clip.webm")?;
    ensure!(head["status"] == 206 && head["size"] == 100, "head: {head}");
    ensure!(
        head["range"] == format!("bytes 0-99/{total}").as_str(),
        "head: {head}"
    );
    ensure!(head["length"] == "100", "head: {head}");
    ensure!(head["body"] == json!(&clip[..64]), "head bytes differ");

    let open = probe(
        &mut view,
        Some(&format!("bytes={}-", total - 10)),
        "clip.webm",
    )?;
    ensure!(
        open["status"] == 206 && open["size"] == 10 && open["length"] == "10",
        "open: {open}"
    );
    ensure!(
        open["range"] == format!("bytes {}-{}/{total}", total - 10, total - 1).as_str(),
        "open: {open}"
    );

    let tail = probe(&mut view, Some("bytes=-50"), "clip.webm")?;
    ensure!(
        tail["status"] == 206 && tail["size"] == 50 && tail["length"] == "50",
        "tail: {tail}"
    );
    ensure!(
        tail["range"] == format!("bytes {}-{}/{total}", total - 50, total - 1).as_str(),
        "tail: {tail}"
    );

    let beyond = probe(&mut view, Some(&format!("bytes={total}-")), "clip.webm")?;
    ensure!(beyond["status"] == 416, "beyond: {beyond}");
    ensure!(
        beyond["range"] == format!("bytes */{total}").as_str(),
        "beyond: {beyond}"
    );

    let missing = probe(&mut view, None, "absent.png")?;
    ensure!(missing["status"] == 404, "missing: {missing}");
    view.close()
}

fn check_frames(view: &mut View, frames: &[u32], canvas: (u32, u32), at: u32) -> Result<()> {
    for &wanted in frames {
        let state = view.evaluate(&format!(
            r#"(async () => {{
              const video = document.getElementById('clip');
              if (video.readyState < 1) {{
                await new Promise((resolve) => video.addEventListener('loadedmetadata', resolve, {{once: true}}));
              }}
              const seeked = new Promise((resolve) => video.addEventListener('seeked', resolve, {{once: true}}));
              video.currentTime = ({wanted} + 0.5) / 10;
              await seeked;
              const canvas = document.createElement('canvas');
              canvas.width = {width};
              canvas.height = {height};
              const context = canvas.getContext('2d');
              context.drawImage(video, 0, 0);
              const [r, g, b] = context.getImageData({at}, {at}, 1, 1).data;
              let best = 0;
              let distance = Infinity;
              for (let frame = 0; frame < 10; frame++) {{
                const d = Math.abs(r - (20 + 24 * frame)) + Math.abs(g - (235 - 24 * frame)) + Math.abs(b - 128);
                if (d < distance) {{ distance = d; best = frame; }}
              }}
              return {{frame: best, distance, width: video.videoWidth}};
            }})()"#,
            width = canvas.0,
            height = canvas.1,
        ))?;
        ensure!(
            state["frame"] == wanted % 10 && state["width"] == canvas.0,
            "asked for frame {wanted}: {state}"
        );
        ensure!(
            state["distance"].as_f64().unwrap_or(f64::MAX) < 30.0,
            "frame {wanted} colour is off: {state}"
        );
    }
    Ok(())
}

fn video_seeks_to_frame(ui: &Ui) -> Result<()> {
    let dir = scratch("video", "http://unused.invalid/lib.js")?;
    let project = Project::open(&dir, Mode::Offline)?;
    let mut view = View::open_project(ui, &project, "video.html", 320, 240, None)?;
    check_frames(&mut view, &[7, 2, 9], (64, 64), 32)?;
    view.close()
}

fn video_seeks_across_ranged_requests(ui: &Ui) -> Result<()> {
    let dir = scratch("ranged", "http://unused.invalid/lib.js")?;
    let filter = "color=c=black:s=320x240:r=10:d=10,format=gbrp,\
        geq=r='if(lt(X,32)*lt(Y,32),20+24*mod(N,10),random(1)*255)':\
        g='if(lt(X,32)*lt(Y,32),235-24*mod(N,10),random(1)*255)':\
        b='if(lt(X,32)*lt(Y,32),128,random(1)*255)',\
        scale=out_color_matrix=bt709:out_range=tv,format=yuv420p";
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", filter])
        .args(["-c:v", "libvpx-vp9", "-crf", "20", "-b:v", "0", "-g", "10"])
        .args(["-deadline", "realtime", "-cpu-used", "8"])
        .args([
            "-colorspace",
            "bt709",
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
        ])
        .arg(dir.join("clip.webm"))
        .status()?;
    ensure!(status.success(), "ffmpeg could not make the clip");
    ensure!(
        fs::metadata(dir.join("clip.webm"))?.len() > 1 << 20,
        "the clip is too small for Chromium to need more than one request"
    );
    let project = Project::open(&dir, Mode::Offline)?;
    let mut view = View::open_project(ui, &project, "video.html", 320, 240, None)?;
    check_frames(&mut view, &[87, 3, 99, 50, 87], (320, 240), 16)?;
    view.close()
}

fn render_fails_on_late_unpinned_request(ui: &Ui) -> Result<()> {
    let cdn = Cdn::start()?;
    let url = cdn.url("/late.js");
    let dir = scratch("late", &url)?;
    let project = Project::open(&dir, Mode::Offline)?;
    let mut view = View::open_project(ui, &project, "late.html", 320, 240, None)?;
    let config = Config::new(320, 240, Rational::new(30, 1), Codec::H264, 5);
    let out = dir.join("out.mp4");
    let Err(error) = render(
        &mut view,
        &config,
        |view, frame| view.evaluate(&format!("seek({frame})")).map(drop),
        &out,
    ) else {
        bail!("a render with an unpinned request succeeded");
    };
    let message = format!("{error:#}");
    ensure!(
        message.contains(&url) && message.contains("not pinned"),
        "unclear failure: {message}"
    );
    ensure!(cdn.hits() == 0, "offline mode sent {} requests", cdn.hits());
    view.close()
}

fn main() -> ExitCode {
    let code = support::run(&[
        ("online_pins_and_renders", online_pins_and_renders),
        (
            "offline_serves_pinned_without_network",
            offline_serves_pinned_without_network,
        ),
        (
            "offline_serves_https_pin_from_lock",
            offline_serves_https_pin_from_lock,
        ),
        ("offline_rejects_unpinned", offline_rejects_unpinned),
        (
            "offline_rejects_tampered_cache",
            offline_rejects_tampered_cache,
        ),
        ("origin_behaves_like_https", origin_behaves_like_https),
        (
            "range_requests_return_partial_content",
            range_requests_return_partial_content,
        ),
        ("video_seeks_to_frame", video_seeks_to_frame),
        (
            "video_seeks_across_ranged_requests",
            video_seeks_across_ranged_requests,
        ),
        (
            "render_fails_on_late_unpinned_request",
            render_fails_on_late_unpinned_request,
        ),
    ]);
    let _ = fs::remove_dir_all(scratch_root());
    code
}
