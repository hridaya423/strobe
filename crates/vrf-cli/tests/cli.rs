use std::fs;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use vrf_bench::corpus::bench_dir;
use vrf_blink::daemon::{self, Job, Reply, Request};

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../vrf-blink/js/test/remotion/fixtures"
);
const VRF: &str = env!("CARGO_BIN_EXE_strobe");
const WARM_BAR_MS: f64 = 500.0;

#[cfg(windows)]
fn symlink(target: PathBuf, link: &Path) -> std::io::Result<()> {
    let target = fs::canonicalize(target)?;
    let target = target
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_string();
    let status = Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(std::path::absolute(link)?)
        .arg(target)
        .stdout(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other("mklink /J failed"))
    }
}

type Case = (&'static str, fn() -> Result<()>);

fn work_dir() -> PathBuf {
    std::env::temp_dir().join("vrf-cli-tests")
}

fn fixture_project(name: &str) -> Result<PathBuf> {
    let project = work_dir().join(name);
    let modules = project.join("node_modules");
    if !modules.exists() {
        fs::create_dir_all(&project)?;
        fs::write(
            project.join("package.json"),
            format!(r#"{{"name":"vrf-{name}","private":true}}"#),
        )?;
        let reference = std::env::var_os("VRF_BENCH_DIR")
            .map_or_else(bench_dir, PathBuf::from)
            .join("corpus/external/github-unwrapped/node_modules");
        let _ = fs::remove_file(&modules);
        symlink(reference, &modules)?;
    }
    let src = project.join("src");
    fs::create_dir_all(&src)?;
    fs::copy(
        Path::new(FIXTURES).join(name).join("index.jsx"),
        src.join("index.jsx"),
    )?;
    Ok(project)
}

fn hyperframes_package() -> PathBuf {
    bench_dir().join("engines/node_modules/hyperframes")
}

fn hyperframes_project() -> Result<PathBuf> {
    let dir = work_dir().join("hyperframes-box");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir)?;
    fs::copy(
        hyperframes_package().join("dist/vendor/npm/gsap@3.15.0/dist/gsap.min.js"),
        dir.join("gsap.min.js"),
    )?;
    fs::write(
        dir.join("index.html"),
        "<!doctype html>\n<html><head><meta charset=\"utf-8\"><style>\
         html,body{margin:0;background:#123}\
         #root{position:relative;width:320px;height:180px;overflow:hidden;background:#123}\
         </style></head><body>\n\
         <div id=\"root\" data-composition-id=\"main\" data-width=\"320\" data-height=\"180\" \
         data-duration=\"1\" data-start=\"0\" data-fps=\"30\">\
         <div id=\"box\" style=\"position:absolute;left:0;top:60px;width:60px;height:60px;background:#f84\"></div></div>\n\
         <script src=\"gsap.min.js\"></script>\n\
         <script>const tl = gsap.timeline({paused: true}); \
         tl.to('#box', {x: 240, duration: 1, ease: 'none'}); \
         window.__timelines = {main: tl};</script></body></html>\n",
    )?;
    Ok(dir)
}

fn socket_dir() -> PathBuf {
    if cfg!(unix) {
        PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    }
}

fn socket() -> PathBuf {
    socket_dir().join(format!("vrf-cli-{}.sock", std::process::id()))
}

fn vrf(socket: &Path) -> Command {
    let mut command = Command::new(VRF);
    command
        .env("VRF_SOCKET", socket)
        .env_remove("VRF_NO_DAEMON")
        .env_remove("VRF_COMMAND_START_US");
    command
}

fn render_args(project: &Path, composition: &str, out: &Path) -> Vec<String> {
    [
        "render",
        project.to_str().unwrap(),
        "--composition",
        composition,
        "--entry",
        "src/index.jsx",
        "--mode",
        "reference",
        "--offline",
        "--timeout",
        "2500",
        "--views",
        "1",
        "--run",
        "1000",
        "--json",
        "-o",
        out.to_str().unwrap(),
    ]
    .map(String::from)
    .to_vec()
}

fn render(
    socket: &Path,
    project: &Path,
    composition: &str,
    out: &Path,
    extra: &[&str],
) -> Result<Value> {
    let output = vrf(socket)
        .args(render_args(project, composition, out))
        .args(extra)
        .output()?;
    ensure!(
        output.status.success(),
        "strobe render failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn render_hyperframes(
    socket: &Path,
    project: &Path,
    out: &Path,
    extra: &[&str],
) -> Result<(Value, String)> {
    let output = vrf(socket)
        .args(["render", project.to_str().unwrap(), "--hyperframes"])
        .arg(hyperframes_package())
        .args([
            "--views",
            "1",
            "--run",
            "1000",
            "--no-turbo",
            "--json",
            "-o",
        ])
        .arg(out)
        .args(extra)
        .output()?;
    ensure!(
        output.status.success(),
        "strobe render failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok((
        serde_json::from_slice(&output.stdout)?,
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

fn frames_hash(path: &Path) -> Result<String> {
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "framemd5", "-"])
        .output()?;
    ensure!(
        output.status.success(),
        "ffmpeg cannot decode {}",
        path.display()
    );
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.rsplit(',').next())
        .map(|hash| hash.trim().to_owned())
        .collect::<Vec<_>>()
        .join(","))
}

struct Running {
    socket: PathBuf,
    child: Child,
    log: PathBuf,
}

impl Running {
    fn start(socket: &Path, tag: &str, extra: &[&str]) -> Result<Running> {
        let log = work_dir().join(format!("daemon-{tag}.log"));
        fs::create_dir_all(work_dir())?;
        let child = vrf(socket)
            .args(["daemon", "--mode", "reference"])
            .args(extra)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(&log)?)
            .spawn()?;
        let running = Running {
            socket: socket.to_owned(),
            child,
            log,
        };
        let deadline = Instant::now() + Duration::from_secs(120);
        while !fs::read_to_string(&running.log)?.contains("ready on") {
            ensure!(Instant::now() < deadline, "the daemon never became ready");
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(running)
    }

    fn ping(&self) -> Result<()> {
        let stream = daemon::connect(&self.socket).context("nothing listens")?;
        let request = Request::new(true, std::time::SystemTime::now(), Job::Ping)?;
        match daemon::call(stream, &request, &mut |_| {}, &mut |_, _| {})? {
            Reply::Pong => Ok(()),
            _ => bail!("no pong"),
        }
    }

    fn stop(mut self) -> Result<()> {
        let output = vrf(&self.socket).args(["daemon", "--stop"]).output()?;
        ensure!(output.status.success(), "strobe daemon --stop failed");
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = self.child.try_wait()? {
                ensure!(status.success(), "the daemon exited with {status}");
                break;
            }
            ensure!(
                Instant::now() < deadline,
                "the daemon did not exit after --stop"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        ensure!(!self.socket.exists(), "the socket file was left behind");
        Ok(())
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = vrf(&self.socket).args(["daemon", "--stop"]).output();
            let deadline = Instant::now() + Duration::from_secs(15);
            while self.child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
            if self.child.try_wait().ok().flatten().is_none() {
                let _ = self.child.kill();
            }
        }
    }
}

fn fallback_without_a_daemon() -> Result<()> {
    let project = fixture_project("gray")?;
    let out = work_dir().join("fallback.mp4");
    let report = render(
        &socket_dir().join("vrf-cli-nothing.sock"),
        &project,
        "Gray",
        &out,
        &[],
    )?;
    ensure!(report["frames"] == 60);
    ensure!(report["stages"]["warm"]["view"] == false);
    ensure!(report["first_frame_ms"].as_f64().unwrap_or(0.0) > 0.0);
    ensure!(report["setup_ms"].as_f64().unwrap_or(0.0) > 0.0);
    println!(
        "no daemon: first_frame_ms {:.0}, setup_ms {:.0}, total_ms {:.0}",
        report["first_frame_ms"].as_f64().unwrap(),
        report["setup_ms"].as_f64().unwrap(),
        report["total_ms"].as_f64().unwrap()
    );
    ensure!(frames_hash(&out)?.split(',').count() == 60);
    Ok(())
}

fn warm_daemon_is_fast_and_matches_the_fallback() -> Result<()> {
    let project = fixture_project("gray")?;
    let socket = socket();
    let daemon = Running::start(&socket, "warm", &["--idle-timeout", "300"])?;
    let work = work_dir();
    let cold = render(&socket, &project, "Gray", &work.join("cli-cold.mp4"), &[])?;
    daemon.ping()?;
    let warm = render(&socket, &project, "Gray", &work.join("cli-warm.mp4"), &[])?;
    let local = render(
        &socket,
        &project,
        "Gray",
        &work.join("cli-local.mp4"),
        &["--no-daemon"],
    )?;
    println!(
        "cli: daemon cold first_frame_ms {:.0}, daemon warm {:.0} (setup {:.0}), in process {:.0}",
        cold["first_frame_ms"].as_f64().unwrap(),
        warm["first_frame_ms"].as_f64().unwrap(),
        warm["setup_ms"].as_f64().unwrap(),
        local["first_frame_ms"].as_f64().unwrap()
    );
    ensure!(warm["stages"]["warm"]["view"] == true, "{}", warm["stages"]);
    ensure!(
        warm["first_frame_ms"].as_f64().unwrap() < WARM_BAR_MS,
        "first_frame_ms {} is not under {WARM_BAR_MS}",
        warm["first_frame_ms"]
    );
    ensure!(local["stages"]["warm"]["view"] == false);
    let expected = frames_hash(&work.join("cli-local.mp4"))?;
    ensure!(frames_hash(&work.join("cli-warm.mp4"))? == expected);
    ensure!(frames_hash(&work.join("cli-cold.mp4"))? == expected);
    daemon.stop()
}

fn two_clients_queue_and_both_finish() -> Result<()> {
    let project = fixture_project("gray")?;
    let socket = socket();
    let daemon = Running::start(&socket, "queue", &["--idle-timeout", "300"])?;
    let work = work_dir();
    let spawn = |name: &str| -> Result<Child> {
        Ok(vrf(&socket)
            .args(render_args(
                &project,
                "Gray",
                &work.join(format!("cli-q-{name}.mp4")),
            ))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?)
    };
    let (first, second) = (spawn("a")?, spawn("b")?);
    let (first, second) = (first.wait_with_output()?, second.wait_with_output()?);
    for output in [&first, &second] {
        ensure!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let waited = [&first, &second]
        .iter()
        .any(|o| String::from_utf8_lossy(&o.stderr).contains("render(s) ahead"));
    ensure!(waited, "neither client reported waiting");
    ensure!(frames_hash(&work.join("cli-q-a.mp4"))? == frames_hash(&work.join("cli-q-b.mp4"))?);
    daemon.stop()
}

fn one_daemon_serves_remotion_then_hyperframes() -> Result<()> {
    let gray = fixture_project("gray")?;
    let boxes = hyperframes_project()?;
    let socket = socket();
    let fast_socket = PathBuf::from(format!("{}-fast", socket.display()));
    let daemon = Running::start(&socket, "mixed", &["--idle-timeout", "300"])?;
    let work = work_dir();

    let remotion = render(
        &socket,
        &gray,
        "Gray",
        &work.join("mixed-remotion.mp4"),
        &[],
    )?;
    ensure!(remotion["frames"] == 60);
    ensure!(
        daemon::connect(&fast_socket).is_none(),
        "a Remotion render started the GPU-raster daemon"
    );

    let (first, first_note) =
        render_hyperframes(&socket, &boxes, &work.join("mixed-hf-first.mp4"), &[])?;
    ensure!(first["frames"] == 30);
    ensure!(
        first_note.contains("starting a fast daemon"),
        "no daemon was started for HyperFrames:\n{first_note}"
    );
    ensure!(
        daemon::connect(&fast_socket).is_some(),
        "no GPU-raster daemon listens on {}",
        fast_socket.display()
    );

    let (warm, warm_note) =
        render_hyperframes(&socket, &boxes, &work.join("mixed-hf-warm.mp4"), &[])?;
    let (local, _) = render_hyperframes(
        &socket,
        &boxes,
        &work.join("mixed-hf-local.mp4"),
        &["--no-daemon"],
    )?;
    println!(
        "mixed: HyperFrames first_frame_ms daemon cold {:.0}, daemon warm {:.0}, in process {:.0}",
        first["first_frame_ms"].as_f64().unwrap(),
        warm["first_frame_ms"].as_f64().unwrap(),
        local["first_frame_ms"].as_f64().unwrap()
    );
    ensure!(!warm_note.contains("starting"), "{warm_note}");
    ensure!(
        warm["first_frame_ms"].as_f64().unwrap() < local["first_frame_ms"].as_f64().unwrap(),
        "the second HyperFrames render did not start faster than an in-process one"
    );
    let expected = frames_hash(&work.join("mixed-hf-local.mp4"))?;
    ensure!(expected.split(',').count() == 30);
    ensure!(frames_hash(&work.join("mixed-hf-first.mp4"))? == expected);
    ensure!(frames_hash(&work.join("mixed-hf-warm.mp4"))? == expected);

    let again = render(
        &socket,
        &gray,
        "Gray",
        &work.join("mixed-remotion-2.mp4"),
        &[],
    )?;
    ensure!(
        again["stages"]["warm"]["view"] == true,
        "{}",
        again["stages"]
    );
    ensure!(
        frames_hash(&work.join("mixed-remotion-2.mp4"))?
            == frames_hash(&work.join("mixed-remotion.mp4"))?
    );

    daemon.stop()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while fast_socket.exists() {
        ensure!(
            Instant::now() < deadline,
            "--stop left the GPU-raster daemon running"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

fn failed_render_leaves_the_daemon_serving() -> Result<()> {
    let boom = fixture_project("throws")?;
    let gray = fixture_project("gray")?;
    let socket = socket();
    let daemon = Running::start(&socket, "fail", &["--idle-timeout", "300"])?;
    let work = work_dir();
    let output = vrf(&socket)
        .args(render_args(&boom, "Boom", &work.join("cli-boom.mp4")))
        .output()?;
    ensure!(!output.status.success(), "a page that throws rendered");
    ensure!(
        String::from_utf8_lossy(&output.stderr).contains("boom at frame 5"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let after = render(&socket, &gray, "Gray", &work.join("cli-after.mp4"), &[])?;
    ensure!(after["frames"] == 60);
    daemon.stop()
}

fn idle_daemon_exits_by_itself() -> Result<()> {
    let socket = socket();
    let mut daemon = Running::start(&socket, "idle", &["--idle-timeout", "2"])?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = daemon.child.try_wait()? {
            ensure!(status.success(), "{status}");
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "the idle daemon is still running"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    ensure!(!socket.exists(), "the socket file was left behind");
    Ok(())
}

fn main() -> ExitCode {
    let cases: [Case; 6] = [
        ("cli_falls_back_without_a_daemon", fallback_without_a_daemon),
        (
            "cli_warm_daemon_is_fast_and_matches_the_fallback",
            warm_daemon_is_fast_and_matches_the_fallback,
        ),
        (
            "cli_two_clients_queue_and_both_finish",
            two_clients_queue_and_both_finish,
        ),
        (
            "cli_one_daemon_serves_remotion_then_hyperframes",
            one_daemon_serves_remotion_then_hyperframes,
        ),
        (
            "cli_failed_render_leaves_the_daemon_serving",
            failed_render_leaves_the_daemon_serving,
        ),
        (
            "cli_idle_daemon_exits_by_itself",
            idle_daemon_exits_by_itself,
        ),
    ];
    let filters: Vec<String> = std::env::args()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .collect();
    let mut failed = 0;
    let mut passed = 0;
    println!("running tests");
    for (name, case) in cases {
        if !filters.is_empty() && !filters.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        match case() {
            Ok(()) => {
                passed += 1;
                println!("test {name} ... ok");
            }
            Err(error) => {
                failed += 1;
                println!("test {name} ... FAILED\n{error:#}");
            }
        }
    }
    println!(
        "\ntest result: {}. {passed} passed; {failed} failed",
        if failed == 0 { "ok" } else { "FAILED" }
    );
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
