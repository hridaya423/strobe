use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use ring::digest;
use serde::{Deserialize, Serialize};

use crate::Ui;
use crate::adapters::hyperframes;
use crate::adapters::remotion::{FrameAssets, RenderJob, Renderer, Report};
use transport::{Auth, Listener, Stream};

const BUILD_ID_BYTES: u64 = 64 * 1024;

#[cfg(unix)]
mod transport {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};

    use anyhow::{Context, Result, bail};

    pub type Stream = UnixStream;
    pub type Listener = UnixListener;

    pub fn socket_path(mode: &str) -> PathBuf {
        let user = unsafe { libc::getuid() };
        std::env::temp_dir().join(format!("vrf-daemon-{user}{mode}.sock"))
    }

    pub fn connect(socket: &Path) -> Option<Stream> {
        UnixStream::connect(socket).ok()
    }

    pub fn bind(socket: &Path) -> Result<Listener> {
        if socket.exists() {
            if UnixStream::connect(socket).is_ok() {
                bail!("a daemon already listens on {}", socket.display());
            }
            fs::remove_file(socket)?;
        }
        let listener = UnixListener::bind(socket)
            .with_context(|| format!("listen on {}", socket.display()))?;
        fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
        Ok(listener)
    }

    pub fn incoming(listener: &Listener) -> impl Iterator<Item = std::io::Result<Stream>> {
        listener.incoming()
    }

    #[derive(Clone)]
    pub struct Auth;

    impl Auth {
        pub fn of(_: &Listener) -> Auth {
            Auth
        }

        pub fn check(&self, _: &mut std::io::BufReader<Stream>) -> bool {
            true
        }
    }

    pub fn remove(socket: &Path) {
        let _ = fs::remove_file(socket);
    }
}

#[cfg(windows)]
mod transport {
    use std::fs;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{Ipv4Addr, TcpListener, TcpStream};
    use std::path::{Path, PathBuf};

    use anyhow::{Context, Result, bail};
    use ring::rand::{SecureRandom, SystemRandom};

    pub type Stream = TcpStream;

    pub struct Listener {
        inner: TcpListener,
        token: String,
    }

    pub fn socket_path(mode: &str) -> PathBuf {
        std::env::temp_dir().join(format!("vrf-daemon{mode}.port"))
    }

    fn read(socket: &Path) -> Option<(u16, String)> {
        let text = fs::read_to_string(socket).ok()?;
        let (port, token) = text.trim().split_once(' ')?;
        Some((port.parse().ok()?, token.to_owned()))
    }

    pub fn connect(socket: &Path) -> Option<Stream> {
        let (port, token) = read(socket)?;
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).ok()?;
        stream.write_all(format!("{token}\n").as_bytes()).ok()?;
        Some(stream)
    }

    pub fn bind(socket: &Path) -> Result<Listener> {
        if connect(socket).is_some() {
            bail!("a daemon already listens on {}", socket.display());
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).context("listen on loopback")?;
        let mut bytes = [0u8; 16];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| anyhow::anyhow!("no random bytes"))?;
        let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let port = listener.local_addr()?.port();
        fs::write(socket, format!("{port} {token}"))?;
        Ok(Listener {
            inner: listener,
            token,
        })
    }

    pub fn incoming(listener: &Listener) -> impl Iterator<Item = std::io::Result<Stream>> {
        listener.inner.incoming()
    }

    #[derive(Clone)]
    pub struct Auth(String);

    impl Auth {
        pub fn of(listener: &Listener) -> Auth {
            Auth(listener.token.clone())
        }

        pub fn check(&self, reader: &mut BufReader<Stream>) -> bool {
            let mut line = String::new();
            reader.read_line(&mut line).is_ok() && line.trim() == self.0
        }
    }

    pub fn remove(socket: &Path) {
        let _ = fs::remove_file(socket);
    }
}

/// Where the daemon listens: `VRF_SOCKET`, or a socket in the temp directory. A process that runs
/// WebGL on SwiftShader has a socket of its own, because a running CEF cannot change that. On
/// Windows the file holds the loopback port and a token that the daemon expects as a connection's
/// first line.
pub fn socket_path() -> PathBuf {
    std::env::var_os("VRF_SOCKET").map_or_else(
        || {
            let mode = if crate::software_webgl() {
                "-swiftshader"
            } else {
                ""
            };
            transport::socket_path(mode)
        },
        PathBuf::from,
    )
}

/// Identifies the running binary: its length and a hash of its first 64 KiB, which holds the
/// Mach-O header with the linker's content UUID. A daemon only serves clients with the same id,
/// because they share a wire format and a CEF build.
pub fn build_id() -> Result<String> {
    let exe = std::env::current_exe()?;
    let length = fs::metadata(&exe)?.len();
    let mut head = Vec::new();
    File::open(&exe)?
        .take(BUILD_ID_BYTES)
        .read_to_end(&mut head)?;
    let hash = digest::digest(&digest::SHA256, &head);
    let hex: String = hash.as_ref()[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(format!("{}-{length}-{hex}", env!("CARGO_PKG_VERSION")))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Job {
    /// Answers once every job before it has finished.
    Ping,
    /// Finishes the jobs before it, then exits.
    Stop,
    Render(RenderJob),
    Hyperframes(hyperframes::RenderJob),
}

/// One line of JSON from client to daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub build: String,
    /// Whether the client wants software rasterisation, which a running CEF cannot change.
    pub cpu_raster: bool,
    /// Whether the client runs WebGL on SwiftShader, which a running CEF cannot change either.
    pub software_webgl: bool,
    /// When the user's command began, in microseconds since the Unix epoch.
    pub command_start_us: u64,
    pub job: Job,
}

impl Request {
    pub fn new(cpu_raster: bool, command_start: SystemTime, job: Job) -> Result<Request> {
        Ok(Request {
            build: build_id()?,
            cpu_raster,
            software_webgl: crate::software_webgl(),
            command_start_us: command_start.duration_since(UNIX_EPOCH)?.as_micros() as u64,
            job,
        })
    }
}

/// One line of JSON from daemon to client.
#[derive(Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Event {
    Queued {
        ahead: usize,
    },
    Progress {
        frame: u64,
        frames: u64,
    },
    Done {
        report: Box<Report>,
        assets: Vec<FrameAssets>,
    },
    DoneHyperframes {
        report: Box<hyperframes::Report>,
    },
    Pong,
    Refused {
        reason: String,
    },
    Failed {
        message: String,
    },
}

fn send(mut stream: &Stream, event: &Event) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(event).expect("an event serialises");
    line.push(b'\n');
    stream.write_all(&line)
}

/// How a daemon answered.
pub enum Reply {
    Rendered(Box<Report>, Vec<FrameAssets>),
    RenderedHyperframes(Box<hyperframes::Report>),
    Pong,
    /// The daemon will not serve this request, such as one from another build. Render in
    /// process instead.
    Refused(String),
    /// The daemon closed the connection before it said anything, such as one that was exiting.
    Gone,
}

/// Connects to the daemon at `socket`, if one is listening.
pub fn connect(socket: &Path) -> Option<Stream> {
    transport::connect(socket)
}

/// Sends `request` and follows the daemon's events to the end. `queued` gets the number of
/// renders ahead of this one, and `progress` the frames done and the total. Fails if the render
/// failed, or the daemon vanished during it.
pub fn call(
    stream: Stream,
    request: &Request,
    queued: &mut dyn FnMut(usize),
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Reply> {
    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    (&stream).write_all(&line).context("send the request")?;
    let mut lines = BufReader::new(stream);
    let mut heard = false;
    loop {
        let mut text = String::new();
        if lines.read_line(&mut text)? == 0 {
            if heard {
                bail!("the daemon closed the connection before the render finished");
            }
            return Ok(Reply::Gone);
        }
        heard = true;
        match serde_json::from_str(&text).context("the daemon sent an unreadable event")? {
            Event::Queued { ahead } => queued(ahead),
            Event::Progress { frame, frames } => progress(frame, frames),
            Event::Done { report, assets } => return Ok(Reply::Rendered(report, assets)),
            Event::DoneHyperframes { report } => return Ok(Reply::RenderedHyperframes(report)),
            Event::Pong => return Ok(Reply::Pong),
            Event::Refused { reason } => return Ok(Reply::Refused(reason)),
            Event::Failed { message } => bail!("{message}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub socket: PathBuf,
    /// The daemon exits after this long with nothing to do.
    pub idle: Duration,
    pub cpu_raster: bool,
    /// How many projects keep their bundle and frame server.
    pub projects: usize,
}

struct Queued {
    request: Request,
    stream: Stream,
}

fn accept(
    listener: Listener,
    build: String,
    cpu_raster: bool,
    pending: Arc<AtomicUsize>,
    jobs: mpsc::Sender<Queued>,
    stopping: Arc<AtomicBool>,
) {
    let auth = Auth::of(&listener);
    for stream in transport::incoming(&listener) {
        if stopping.load(Ordering::Acquire) {
            return;
        }
        let Ok(stream) = stream else { continue };
        let (build, pending, jobs) = (build.clone(), pending.clone(), jobs.clone());
        let auth = auth.clone();
        thread::spawn(move || {
            let Ok(copy) = stream.try_clone() else { return };
            let mut reader = BufReader::new(copy);
            if !auth.check(&mut reader) {
                return;
            }
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                return;
            }
            let request = match serde_json::from_str::<Request>(&line) {
                Ok(request) => request,
                Err(error) => {
                    let _ = send(
                        &stream,
                        &Event::Failed {
                            message: format!("unreadable request: {error}"),
                        },
                    );
                    return;
                }
            };
            let renders = matches!(request.job, Job::Render(_) | Job::Hyperframes(_));
            let refusal = if renders && request.build != build {
                Some(format!(
                    "the daemon is build {build} and the client is {}; restart the daemon",
                    request.build
                ))
            } else if renders && request.cpu_raster != cpu_raster {
                Some(format!(
                    "the daemon rasterises on the {}; start one with the mode you need",
                    if cpu_raster { "CPU" } else { "GPU" }
                ))
            } else if renders && request.software_webgl != crate::software_webgl() {
                Some(format!(
                    "the daemon runs WebGL on {}; start one for the WebGL mode you need",
                    if request.software_webgl {
                        "the GPU"
                    } else {
                        "SwiftShader"
                    },
                ))
            } else {
                None
            };
            if let Some(reason) = refusal {
                let _ = send(&stream, &Event::Refused { reason });
                return;
            }
            let ahead = pending.fetch_add(1, Ordering::SeqCst);
            if ahead > 0 {
                let _ = send(&stream, &Event::Queued { ahead });
            }
            let _ = jobs.send(Queued { request, stream });
        });
    }
}

fn finish<T>(
    renderer: &mut Renderer,
    outcome: std::thread::Result<Result<T>>,
    done: impl FnOnce(T) -> Event,
) -> Event {
    match outcome {
        Ok(Ok(report)) => done(report),
        Ok(Err(error)) => Event::Failed {
            message: format!("{error:#}"),
        },
        Err(_) => {
            renderer.clear();
            Event::Failed {
                message: "the render panicked".to_owned(),
            }
        }
    }
}

fn render(
    renderer: &mut Renderer,
    ui: &Ui,
    job: RenderJob,
    command_start: SystemTime,
    stream: &Stream,
) -> Event {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        renderer.render(ui, &job, command_start, &mut |frame, frames| {
            send(stream, &Event::Progress { frame, frames }).context("the client went away")
        })
    }));
    finish(renderer, outcome, |mut report| {
        let assets = std::mem::take(&mut report.assets);
        Event::Done {
            report: Box::new(report),
            assets,
        }
    })
}

fn render_hyperframes(
    renderer: &mut Renderer,
    ui: &Ui,
    job: hyperframes::RenderJob,
    command_start: SystemTime,
    stream: &Stream,
) -> Event {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        renderer.render_hyperframes(ui, &job, command_start, &mut |frame, frames| {
            send(stream, &Event::Progress { frame, frames }).context("the client went away")
        })
    }));
    finish(renderer, outcome, |report| Event::DoneHyperframes {
        report: Box::new(report),
    })
}

/// Serves render requests on `config.socket` until it has been idle for `config.idle`, or a
/// [`Job::Stop`] arrives. CEF, the GPU state and pixel pools, and the last projects' bundles,
/// frame servers and ready views stay loaded between requests. One render runs at a time and
/// the rest wait in order. A render that fails drops only the project it used.
pub fn serve(ui: &Ui, config: &Config) -> Result<()> {
    let listener = transport::bind(&config.socket)?;
    let build = build_id()?;
    let mut renderer = Renderer::new(config.projects);
    renderer.prepare()?;

    eprintln!("vrf daemon: ready on {}", config.socket.display());
    let pending = Arc::new(AtomicUsize::new(0));
    let stopping = Arc::new(AtomicBool::new(false));
    let (jobs, queue) = mpsc::channel();
    let acceptor = {
        let (pending, stopping) = (pending.clone(), stopping.clone());
        let (build, cpu_raster) = (build, config.cpu_raster);
        thread::spawn(move || accept(listener, build, cpu_raster, pending, jobs, stopping))
    };

    let result = loop {
        let Queued { request, stream } = match queue.recv_timeout(config.idle) {
            Ok(queued) => queued,
            Err(RecvTimeoutError::Timeout) => break Ok(()),
            Err(RecvTimeoutError::Disconnected) => break Err(anyhow::anyhow!("the acceptor died")),
        };
        let command_start = UNIX_EPOCH + Duration::from_micros(request.command_start_us);
        let mut stop = false;
        let reply = match request.job {
            Job::Ping => Event::Pong,
            Job::Stop => {
                stop = true;
                Event::Pong
            }
            Job::Render(job) => render(&mut renderer, ui, job, command_start, &stream),
            Job::Hyperframes(job) => {
                render_hyperframes(&mut renderer, ui, job, command_start, &stream)
            }
        };
        let _ = send(&stream, &reply);
        drop(stream);
        pending.fetch_sub(1, Ordering::SeqCst);
        if stop {
            break Ok(());
        }
        if let Err(error) = renderer.refill(ui) {
            eprintln!("vrf daemon: cannot open a spare view: {error:#}");
        }
    };

    stopping.store(true, Ordering::Release);
    let _ = transport::connect(&config.socket);
    let _ = acceptor.join();
    transport::remove(&config.socket);
    renderer.clear();
    result
}
