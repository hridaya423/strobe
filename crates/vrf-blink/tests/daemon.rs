#[allow(dead_code)]
mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail, ensure};
use support::{fixture_project, load_average, options, work_dir};
use vrf_bench::score;
use vrf_blink::Ui;
use vrf_blink::adapters::remotion::{RenderJob, Renderer, Report};
use vrf_blink::daemon::{self, Config, Job, Reply, Request};
use vrf_encode::Codec;

const WARM_BAR_MS: f64 = 500.0;

struct Daemon {
    socket: PathBuf,
    thread: JoinHandle<Result<()>>,
}

fn start(ui: &Ui, tag: &str, idle: Duration) -> Result<Daemon> {
    let dir = if cfg!(unix) {
        PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    };
    let socket = dir.join(format!("vrf-t-{}-{tag}.sock", std::process::id()));
    let config = Config {
        socket: socket.clone(),
        idle,
        cpu_raster: true,
        projects: 2,
    };
    let ui = ui.clone();
    let thread = thread::spawn(move || daemon::serve(&ui, &config));
    let deadline = Instant::now() + Duration::from_secs(60);
    while daemon::connect(&socket).is_none() {
        ensure!(
            Instant::now() < deadline,
            "the daemon never started listening"
        );
        thread::sleep(Duration::from_millis(20));
    }
    Ok(Daemon { socket, thread })
}

impl Daemon {
    fn call(
        &self,
        job: Job,
        cpu_raster: bool,
        build: Option<&str>,
        queued: &AtomicBool,
    ) -> Result<Reply> {
        let stream = daemon::connect(&self.socket).context("nothing listens")?;
        let mut request = Request::new(cpu_raster, SystemTime::now(), job)?;
        if let Some(build) = build {
            request.build = build.to_owned();
        }
        daemon::call(
            stream,
            &request,
            &mut |_| queued.store(true, Ordering::SeqCst),
            &mut |_, _| {},
        )
    }

    fn render(&self, job: RenderJob) -> Result<Report> {
        match self.call(Job::Render(job), true, None, &AtomicBool::new(false))? {
            Reply::Rendered(report, _) => Ok(*report),
            Reply::Refused(reason) => bail!("refused: {reason}"),
            Reply::Pong | Reply::Gone | Reply::RenderedHyperframes(_) => {
                bail!("no render came back")
            }
        }
    }

    fn ping(&self) -> Result<()> {
        match self.call(Job::Ping, true, None, &AtomicBool::new(false))? {
            Reply::Pong => Ok(()),
            _ => bail!("no pong"),
        }
    }

    fn stop(self) -> Result<()> {
        self.call(Job::Stop, true, None, &AtomicBool::new(false))?;
        self.thread.join().expect("daemon thread panicked")?;
        ensure!(!self.socket.exists(), "the socket file was left behind");
        Ok(())
    }
}

fn job(project: &Path, composition: &str, frames: Option<(u64, u64)>, out: &Path) -> RenderJob {
    RenderJob {
        options: options(project, composition),
        frames: frames.map(|(first, last)| first..=last),
        codec: Codec::H264,
        output: out.to_owned(),
    }
}

fn decoded(path: &Path) -> Result<Vec<Vec<u8>>> {
    score::open(path)?
        .frames
        .map(|frame| Ok(frame?.1.data))
        .collect()
}

fn differing_bytes(a: &Path, b: &Path) -> Result<usize> {
    let (a, b) = (fs::read(a)?, fs::read(b)?);
    Ok(a.len().abs_diff(b.len()) + a.iter().zip(&b).filter(|(x, y)| x != y).count())
}

fn in_process(ui: &Ui, project: &Path, composition: &str, out: &Path) -> Result<Report> {
    Renderer::new(1).render(
        ui,
        &job(project, composition, None, out),
        SystemTime::now(),
        &mut |_, _| Ok(()),
    )
}

fn warm_render_starts_under_the_bar_and_matches_in_process(ui: &Ui) -> Result<()> {
    let project = fixture_project("gray")?;
    let work = work_dir();
    let (cold_out, warm_out, local_out) = (
        work.join("daemon-cold.mp4"),
        work.join("daemon-warm.mp4"),
        work.join("daemon-local.mp4"),
    );
    let daemon = start(ui, "warm", Duration::from_secs(300))?;
    let cold = daemon.render(job(&project, "Gray", None, &cold_out))?;
    daemon.ping()?;
    let warm = daemon.render(job(&project, "Gray", None, &warm_out))?;
    let immediate = daemon.render(job(&project, "Gray", None, &warm_out))?;
    daemon.stop()?;
    let local = in_process(ui, &project, "Gray", &local_out)?;

    println!(
        "gray 60 frames at load {}: in process first_frame_ms {:.0}, daemon cold {:.0}, daemon warm {:.0} (setup {:.0}), warm right after a render {:.0}",
        load_average(),
        local.first_frame_ms,
        cold.first_frame_ms,
        warm.first_frame_ms,
        warm.setup_ms,
        immediate.first_frame_ms
    );
    ensure!(!cold.stages.warm.view && !cold.stages.warm.frame_server);
    ensure!(
        warm.stages.warm.view && warm.stages.warm.frame_server && warm.stages.warm.gpu,
        "the second render did not find a warm view, frame server and GPU: {:?}",
        warm.stages.warm
    );
    ensure!(
        warm.first_frame_ms < WARM_BAR_MS,
        "first_frame_ms {:.0} is not under {WARM_BAR_MS}",
        warm.first_frame_ms
    );
    let reference = decoded(&local_out)?;
    ensure!(reference.len() == 60);
    ensure!(
        decoded(&warm_out)? == reference && decoded(&cold_out)? == reference,
        "a daemon render differs from the in-process render in decoded frames"
    );
    println!(
        "bytes differing between the warm daemon file and the in-process file: {}; between two in-process files: {}",
        differing_bytes(&warm_out, &local_out)?,
        {
            let again = work.join("daemon-local2.mp4");
            in_process(ui, &project, "Gray", &again)?;
            differing_bytes(&again, &local_out)?
        }
    );
    Ok(())
}

fn queued_requests_both_complete(ui: &Ui) -> Result<()> {
    let project = fixture_project("gray")?;
    let work = work_dir();
    let daemon = start(ui, "queue", Duration::from_secs(300))?;
    let saw_queue = AtomicBool::new(false);
    let outcomes: Vec<Result<()>> = thread::scope(|scope| {
        let handles: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|name| {
                let (daemon, project, saw_queue) = (&daemon, &project, &saw_queue);
                let out = work.join(format!("queue-{name}.mp4"));
                scope.spawn(move || -> Result<()> {
                    let reply = daemon.call(
                        Job::Render(job(project, "Gray", None, &out)),
                        true,
                        None,
                        saw_queue,
                    )?;
                    let Reply::Rendered(report, _) = reply else {
                        bail!("no render came back");
                    };
                    ensure!(report.frames == 60);
                    ensure!(decoded(&out)?.len() == 60);
                    Ok(())
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    daemon.stop()?;
    for outcome in outcomes {
        outcome?;
    }
    ensure!(
        saw_queue.load(Ordering::SeqCst),
        "neither client was told it was queued"
    );
    Ok(())
}

fn survives_a_page_error(ui: &Ui) -> Result<()> {
    let boom = fixture_project("throws")?;
    let gray = fixture_project("gray")?;
    let work = work_dir();
    let daemon = start(ui, "throws", Duration::from_secs(300))?;
    daemon.render(job(&gray, "Gray", None, &work.join("survive-gray-1.mp4")))?;
    daemon.ping()?;

    let Err(error) = daemon.render(job(&boom, "Boom", None, &work.join("survive-boom.mp4"))) else {
        bail!("a page that throws rendered");
    };
    let text = format!("{error:#}");
    ensure!(
        text.contains("boom at frame 5"),
        "the error lost the page's message: {text}"
    );

    let after = daemon.render(job(&gray, "Gray", None, &work.join("survive-gray-2.mp4")))?;
    ensure!(after.frames == 60);
    let healthy = daemon.render(job(
        &boom,
        "Boom",
        Some((0, 4)),
        &work.join("survive-boom-ok.mp4"),
    ))?;
    ensure!(healthy.frames == 5);
    ensure!(
        decoded(&work.join("survive-gray-2.mp4"))? == decoded(&work.join("survive-gray-1.mp4"))?,
        "the render after a failure differs from the one before it"
    );
    daemon.stop()
}

#[cfg(windows)]
fn renderer_children() -> Result<Vec<u32>> {
    let script = format!(
        "Get-CimInstance Win32_Process -Filter 'ParentProcessId={}' | Where-Object {{ $_.CommandLine -like '*--type=renderer*' }} | ForEach-Object {{ $_.ProcessId }}",
        std::process::id()
    );
    let output = Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect())
}

#[cfg(windows)]
fn kill(pid: u32) -> std::io::Result<std::process::ExitStatus> {
    Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .status()
}

#[cfg(not(windows))]
fn kill(pid: u32) -> std::io::Result<std::process::ExitStatus> {
    Command::new("kill").args(["-9", &pid.to_string()]).status()
}

#[cfg(not(windows))]
fn renderer_children() -> Result<Vec<u32>> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,ppid=,command="])
        .output()?;
    let ours = std::process::id();
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let (pid, ppid) = (
                parts.next()?.parse::<u32>().ok()?,
                parts.next()?.parse::<u32>().ok()?,
            );
            (ppid == ours && line.contains("--type=renderer")).then_some(pid)
        })
        .collect())
}

fn survives_a_renderer_crash(ui: &Ui) -> Result<()> {
    let gray = fixture_project("gray")?;
    let work = work_dir();
    let daemon = start(ui, "crash", Duration::from_secs(300))?;
    daemon.render(job(&gray, "Gray", None, &work.join("crash-1.mp4")))?;
    daemon.ping()?;
    let victims = renderer_children()?;
    ensure!(!victims.is_empty(), "found no renderer process to kill");
    for pid in &victims {
        let status = kill(*pid)?;
        ensure!(
            status.success() || cfg!(windows),
            "cannot kill renderer {pid}"
        );
    }
    let started = Instant::now();
    let after = daemon.render(job(&gray, "Gray", None, &work.join("crash-2.mp4")))?;
    println!(
        "render after killing {} renderer process(es): view warm {}, first_frame_ms {:.0}, wall {:.0} ms",
        victims.len(),
        after.stages.warm.view,
        after.first_frame_ms,
        started.elapsed().as_secs_f64() * 1000.0
    );
    ensure!(after.frames == 60);
    ensure!(
        decoded(&work.join("crash-2.mp4"))? == decoded(&work.join("crash-1.mp4"))?,
        "the render after a crash differs"
    );
    daemon.stop()
}

fn refuses_another_build_and_another_mode(ui: &Ui) -> Result<()> {
    let gray = fixture_project("gray")?;
    let out = work_dir().join("refused.mp4");
    let daemon = start(ui, "refuse", Duration::from_secs(300))?;
    let render = Job::Render(job(&gray, "Gray", None, &out));
    let queued = AtomicBool::new(false);
    match daemon.call(render.clone(), true, Some("0.0.0-1-deadbeef"), &queued)? {
        Reply::Refused(reason) => ensure!(reason.contains("build"), "{reason}"),
        _ => bail!("a request from another build was served"),
    }
    match daemon.call(render, false, None, &queued)? {
        Reply::Refused(reason) => ensure!(reason.contains("rasterises"), "{reason}"),
        _ => bail!("a request for another raster mode was served"),
    }
    ensure!(!out.exists(), "a refused request wrote its output");
    daemon.ping()?;
    daemon.stop()
}

fn exits_when_idle(ui: &Ui) -> Result<()> {
    let daemon = start(ui, "idle", Duration::from_secs(2))?;
    let socket = daemon.socket.clone();
    let started = Instant::now();
    daemon.thread.join().expect("daemon thread panicked")?;
    ensure!(
        started.elapsed() < Duration::from_secs(10) && !socket.exists(),
        "the idle daemon did not exit and clean up"
    );
    Ok(())
}

fn no_daemon_means_no_connection(_: &Ui) -> Result<()> {
    ensure!(daemon::connect(&std::env::temp_dir().join("vrf-t-nothing-listens.sock")).is_none());
    Ok(())
}

fn main() -> ExitCode {
    vrf_blink::set_cpu_raster(true);
    support::run(&[
        (
            "daemon_warm_render_under_bar_and_matches_in_process",
            warm_render_starts_under_the_bar_and_matches_in_process,
        ),
        (
            "daemon_queued_requests_both_complete",
            queued_requests_both_complete,
        ),
        ("daemon_survives_a_page_error", survives_a_page_error),
        (
            "daemon_survives_a_renderer_crash",
            survives_a_renderer_crash,
        ),
        (
            "daemon_refuses_another_build_and_mode",
            refuses_another_build_and_another_mode,
        ),
        ("daemon_exits_when_idle", exits_when_idle),
        (
            "no_daemon_means_no_connection",
            no_daemon_means_no_connection,
        ),
    ])
}
