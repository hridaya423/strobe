use base64::Engine;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Cursor},
    net::TcpStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use tungstenite::{Message, WebSocket, stream::MaybeTlsStream};

const SHELL: &str = "/Users/hridyaagrawal/.t3/projects/videorenderingframework/bench/engines/browsers/chrome-headless-shell/mac_arm-156.0.8078.73/chrome-headless-shell-mac-arm64/chrome-headless-shell";
const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;

struct Cdp {
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    next: u64,
    session: Option<String>,
}

impl Cdp {
    fn call(&mut self, method: &str, params: Value, in_session: bool) -> Result<Value, String> {
        self.next += 1;
        let id = self.next;
        let mut message = json!({"id": id, "method": method, "params": params});
        if in_session {
            message["sessionId"] = json!(self.session.clone().unwrap());
        }
        self.socket.send(Message::text(message.to_string())).unwrap();
        loop {
            let Message::Text(text) = self.socket.read().unwrap() else { continue };
            let value: Value = serde_json::from_str(&text).unwrap();
            if value["id"] == id {
                return match value.get("error") {
                    Some(error) => Err(error.to_string()),
                    None => Ok(value["result"].clone()),
                };
            }
        }
    }

    fn eval(&mut self, expression: &str) -> Value {
        let result = self
            .call("Runtime.evaluate", json!({"expression": expression, "awaitPromise": true, "returnByValue": true}), true)
            .unwrap();
        assert!(result.get("exceptionDetails").is_none(), "page threw: {result}");
        result["result"]["value"].clone()
    }
}

fn launch(raster: &str, begin_frame: bool, profile: &Path) -> (Child, String) {
    let mut flags: Vec<String> = [
        "--remote-debugging-port=0",
        "--run-all-compositor-stages-before-draw",
        "--disable-threaded-animation",
        "--disable-checker-imaging",
        "--disable-image-animation-resync",
        "--disable-new-content-rendering-timeout",
        "--disable-lcd-text",
        "--force-device-scale-factor=1",
        "--force-color-profile=srgb",
        "--disable-background-timer-throttling",
        "--disable-renderer-backgrounding",
        "--disable-backgrounding-occluded-windows",
        "--hide-scrollbars",
        "--no-sandbox",
        "--use-mock-keychain",
    ]
    .map(String::from)
    .to_vec();
    flags.push(format!("--user-data-dir={}", profile.display()));
    if begin_frame {
        flags.push("--enable-begin-frame-control".into());
    }
    match raster {
        "cpu" => flags.push("--disable-gpu-rasterization".into()),
        "swcomp" => flags.push("--disable-gpu-compositing".into()),
        _ => {}
    }
    let mut child = Command::new(std::env::var("HEADLESS_SHELL").unwrap_or(SHELL.into()))
        .args(&flags)
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("cannot start chrome-headless-shell");
    let stderr = BufReader::new(child.stderr.take().unwrap());
    for line in stderr.lines() {
        let line = line.unwrap();
        if let Some(url) = line.split("DevTools listening on ").nth(1) {
            return (child, url.trim().to_string());
        }
    }
    panic!("chrome-headless-shell did not print a DevTools URL");
}

fn decode_png(bytes: &[u8]) -> Vec<u8> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().unwrap();
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer).unwrap();
    buffer.truncate(info.buffer_size());
    match info.color_type {
        png::ColorType::Rgba => buffer,
        png::ColorType::Rgb => buffer.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        other => panic!("unexpected PNG colour type {other:?}"),
    }
}

fn capture(options: &HashMap<String, String>) {
    let n: u32 = options.get("n").map_or(300, |v| v.parse().unwrap());
    let bf: usize = options.get("bf").map_or(1, |v| v.parse().unwrap());
    let raster = options.get("raster").map_or("cpu", String::as_str);
    let mode = options.get("mode").map_or("beginframe", String::as_str);
    let out = PathBuf::from(options.get("out").unwrap());
    let label = options.get("label").unwrap();
    let page = options.get("page").unwrap();
    std::fs::create_dir_all(out.join("frames")).unwrap();
    let profile = std::env::temp_dir().join(format!("vrf-headless-{}", std::process::id()));
    let (mut child, ws_url) = launch(raster, mode == "beginframe", &profile);
    let (socket, _) = tungstenite::connect(&ws_url).unwrap();
    let mut cdp = Cdp { socket, next: 0, session: None };
    let mut target_params = json!({"url": "about:blank", "width": WIDTH, "height": HEIGHT});
    if mode == "beginframe" {
        target_params["enableBeginFrameControl"] = json!(true);
    }
    let target = cdp.call("Target.createTarget", target_params, false).unwrap();
    let attached = cdp
        .call("Target.attachToTarget", json!({"targetId": target["targetId"], "flatten": true}), false)
        .unwrap();
    cdp.session = Some(attached["sessionId"].as_str().unwrap().to_string());
    cdp.call("Page.enable", json!({}), true).unwrap();
    cdp.call("Emulation.setDeviceMetricsOverride", json!({"width": WIDTH, "height": HEIGHT, "deviceScaleFactor": 1, "mobile": false}), true).unwrap();
    cdp.call("Page.navigate", json!({"url": page}), true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if mode == "beginframe" {
            let _ = cdp.call("HeadlessExperimental.beginFrame", json!({}), true);
        }
        if cdp.eval("document.readyState === 'complete' && typeof seek === 'function'") == json!(true) {
            break;
        }
        assert!(Instant::now() < deadline, "page did not load");
        std::thread::sleep(Duration::from_millis(20));
    }
    cdp.eval("seek(100000)");
    let screenshot = |cdp: &mut Cdp, last: bool| -> Result<Option<Vec<u8>>, String> {
        if mode == "beginframe" {
            let params = if last { json!({"screenshot": {"format": "png"}}) } else { json!({}) };
            let result = cdp.call("HeadlessExperimental.beginFrame", params, true)?;
            Ok(result["screenshotData"].as_str().map(|d| base64::engine::general_purpose::STANDARD.decode(d).unwrap()))
        } else if last {
            let result = cdp.call("Page.captureScreenshot", json!({"format": "png"}), true)?;
            Ok(Some(base64::engine::general_purpose::STANDARD.decode(result["data"].as_str().unwrap()).unwrap()))
        } else {
            Ok(None)
        }
    };
    for _ in 0..3 {
        screenshot(&mut cdp, true).unwrap();
    }
    let mut lines = Vec::new();
    let mut missing = 0;
    let started = Instant::now();
    for i in 0..n {
        cdp.eval(&format!("seek({i})"));
        let mut png = None;
        for j in 0..bf {
            png = screenshot(&mut cdp, j == bf - 1).unwrap_or_else(|e| panic!("capture failed: {e}"));
        }
        let Some(png) = png else {
            missing += 1;
            lines.push(format!("{i} none"));
            continue;
        };
        let rgba = decode_png(&png);
        assert_eq!(rgba.len(), (WIDTH * HEIGHT * 4) as usize, "unexpected screenshot size");
        lines.push(format!("{i} {}", blake3::hash(&rgba).to_hex()));
        std::fs::write(out.join("frames").join(format!("{label}-{i:03}.png")), &png).unwrap();
    }
    std::fs::write(out.join(format!("{label}.hashes")), lines.join("\n") + "\n").unwrap();
    println!(
        "spike=1.5 headless-shell label={label} mode={mode} raster={raster} frames={n} bf={bf} missing={missing} seconds={:.1} load={}",
        started.elapsed().as_secs_f64(),
        load_average()
    );
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(profile);
}

fn load_average() -> String {
    let out = Command::new("uptime").output().unwrap();
    String::from_utf8_lossy(&out.stdout).split("load averages:").nth(1).unwrap_or("?").trim().to_string()
}

fn read_hashes(dir: &Path, label: &str) -> Vec<String> {
    std::fs::read_to_string(dir.join(format!("{label}.hashes")))
        .unwrap()
        .lines()
        .map(|l| l.split_whitespace().nth(1).unwrap().to_string())
        .collect()
}

fn compare(options: &HashMap<String, String>) {
    let (dir_a, dir_b) = (PathBuf::from(&options["dir_a"]), PathBuf::from(&options["dir_b"]));
    let (label_a, label_b) = (&options["label_a"], &options["label_b"]);
    let (a, b) = (read_hashes(&dir_a, label_a), read_hashes(&dir_b, label_b));
    let n = a.len().min(b.len());
    let identical = (0..n).filter(|&i| a[i] == b[i]).count();
    let mut sa: Vec<&String> = a.iter().collect();
    let mut sb: Vec<&String> = b.iter().collect();
    sa.sort();
    sb.sort();
    println!("compare {label_a} vs {label_b}: identical at same index {identical}/{n}; hash multisets equal: {}", sa == sb);
    let (mut worst_delta, mut worst_pixels, mut frames_compared, mut missing_png) = (0u8, 0usize, 0, 0);
    let mut shown = 0;
    for i in 0..n {
        if a[i] == b[i] {
            continue;
        }
        let load = |dir: &Path, label: &str| std::fs::read(dir.join("frames").join(format!("{label}-{i:03}.png"))).ok().map(|bytes| decode_png(&bytes));
        let (Some(x), Some(y)) = (load(&dir_a, label_a), load(&dir_b, label_b)) else {
            missing_png += 1;
            continue;
        };
        frames_compared += 1;
        let pixels = x.chunks_exact(4).zip(y.chunks_exact(4)).filter(|(p, q)| p != q).count();
        let delta = x.iter().zip(&y).map(|(p, q)| p.abs_diff(*q)).max().unwrap();
        worst_delta = worst_delta.max(delta);
        worst_pixels = worst_pixels.max(pixels);
        if shown < 8 {
            shown += 1;
            println!("  frame {i}: differing pixels {pixels} ({:.4}%), max channel delta {delta}", pixels as f64 / (WIDTH * HEIGHT) as f64 * 100.0);
        }
    }
    println!("  differing frames with pixels available: {frames_compared} (no PNG: {missing_png}); worst max delta {worst_delta}; worst differing pixel count {worst_pixels}");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options: HashMap<String, String> = args[1..].iter().filter_map(|a| a.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect();
    match args[0].as_str() {
        "capture" => capture(&options),
        "compare" => compare(&options),
        other => panic!("unknown command {other}"),
    }
}
