use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail, ensure};
use cef::{rc::Rc as _, *};
use percent_encoding::percent_decode_str;
use ring::digest;
use serde::{Deserialize, Serialize};

const LOCK_FILE: &str = "vrf.lock.json";
const CACHE_DIR: &str = ".vrf/cache";
const SCHEME: &str = "vrf";
const HOST: &str = "project";
const ORIGIN: &str = "vrf://project";
const FETCH_TIMEOUT_SECONDS: &str = "60";
const SCHEME_OPTIONS: [sys::cef_scheme_options_t; 4] = [
    sys::cef_scheme_options_t::CEF_SCHEME_OPTION_STANDARD,
    sys::cef_scheme_options_t::CEF_SCHEME_OPTION_SECURE,
    sys::cef_scheme_options_t::CEF_SCHEME_OPTION_CORS_ENABLED,
    sys::cef_scheme_options_t::CEF_SCHEME_OPTION_FETCH_ENABLED,
];

pub(crate) fn register_scheme(registrar: &mut SchemeRegistrar) {
    let options = SCHEME_OPTIONS
        .iter()
        .fold(0, |bits, option| bits | *option as i32);
    registrar.add_custom_scheme(Some(&SCHEME.into()), options);
}

/// How a project treats `http(s)` dependencies that are not in `vrf.lock.json`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Fetch an unpinned URL once, pin it, and serve it from the cache from then on.
    Online,
    /// Serve pinned URLs from the cache and fail on everything else. Nothing reaches the network.
    Offline,
}

#[derive(Serialize, Deserialize, Clone)]
struct Pin {
    url: String,
    sha256: String,
    path: String,
    content_type: String,
}

#[derive(Serialize, Deserialize)]
struct Lock {
    version: u32,
    pins: Vec<Pin>,
}

struct Inner {
    root: PathBuf,
    fallbacks: Vec<PathBuf>,
    mode: Mode,
    lock: Mutex<Lock>,
    failures: Mutex<Vec<String>>,
    direct: Mutex<Vec<String>>,
}

/// A project directory served at `vrf://project/`, with its pinned remote dependencies. Cloning
/// shares the lock and the failure list.
#[derive(Clone)]
pub struct Project(Arc<Inner>);

impl Project {
    /// Opens `root` and reads `vrf.lock.json` if the project has one.
    pub fn open(root: impl AsRef<Path>, mode: Mode) -> Result<Project> {
        Project::open_layers(root, &[], mode)
    }

    /// Opens `root` with directories that serve the files it lacks, tried in order. The lock file
    /// and the cache of pinned dependencies belong to `root`.
    pub fn open_layers(
        root: impl AsRef<Path>,
        fallbacks: &[PathBuf],
        mode: Mode,
    ) -> Result<Project> {
        let canonical = |dir: &Path| {
            dir.canonicalize()
                .with_context(|| format!("project directory {} does not exist", dir.display()))
        };
        let root = canonical(root.as_ref())?;
        let fallbacks = fallbacks
            .iter()
            .map(|dir| canonical(dir))
            .collect::<Result<Vec<_>>>()?;
        let lock = match fs::read(root.join(LOCK_FILE)) {
            Ok(bytes) => {
                let lock: Lock = serde_json::from_slice(&bytes)
                    .with_context(|| format!("{LOCK_FILE} is not valid"))?;
                ensure!(
                    lock.version == 1,
                    "{LOCK_FILE} has unknown version {}",
                    lock.version
                );
                lock
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Lock {
                version: 1,
                pins: Vec::new(),
            },
            Err(error) => return Err(error).context(format!("cannot read {LOCK_FILE}")),
        };
        Ok(Project(Arc::new(Inner {
            root,
            fallbacks,
            mode,
            lock: Mutex::new(lock),
            failures: Mutex::default(),
            direct: Mutex::default(),
        })))
    }

    /// Lets requests whose URL starts with `origin` reach the network without being pinned, for a
    /// server the render itself runs, such as Remotion's frame proxy on `http://localhost:<port>`.
    pub fn allow_direct(&self, origin: String) {
        self.0.direct.lock().unwrap().push(origin);
    }

    fn is_direct(&self, url: &str) -> bool {
        self.0
            .direct
            .lock()
            .unwrap()
            .iter()
            .any(|origin| url.starts_with(origin.as_str()))
    }

    /// The `vrf://project/` URL of a file in the project.
    pub fn url(&self, path: &str) -> String {
        format!("{ORIGIN}/{path}")
    }

    /// Every request the project refused or could not serve so far, one message each. A page that
    /// asked for an unpinned URL offline shows up here.
    pub fn failures(&self) -> Vec<String> {
        self.0.failures.lock().unwrap().clone()
    }

    fn fail(&self, message: String) {
        let mut failures = self.0.failures.lock().unwrap();
        if !failures.contains(&message) {
            failures.push(message);
        }
    }

    pub(crate) fn bind(&self) -> Result<(RequestContext, RequestHandler)> {
        let context = request_context_create_context(Some(&Default::default()), None)
            .context("CEF did not create a request context")?;
        let mut factory = ProjectFactory::new(self.clone());
        ensure!(
            context.register_scheme_handler_factory(
                Some(&SCHEME.into()),
                Some(&HOST.into()),
                Some(&mut factory)
            ) == 1,
            "CEF did not register the {SCHEME} scheme"
        );
        Ok((context, ProjectRequestHandler::new(self.clone())))
    }

    fn respond(&self, url: &str, range: Option<&str>) -> Result<Served> {
        if url.starts_with(ORIGIN) {
            self.local(url, range)
        } else {
            self.remote(url, range)
        }
    }

    fn local(&self, url: &str, range: Option<&str>) -> Result<Served> {
        let Some(relative) = url_path(url) else {
            return Ok(Served::not_found());
        };
        let found = std::iter::once(&self.0.root)
            .chain(&self.0.fallbacks)
            .filter_map(|root| safe_join(root, &relative))
            .map(|path| {
                if path.is_dir() {
                    path.join("index.html")
                } else {
                    path
                }
            })
            .find(|path| path.is_file());
        match found {
            Some(path) => Served::file(&path, mime(&path), range),
            None => Ok(Served::not_found()),
        }
    }

    fn remote(&self, url: &str, range: Option<&str>) -> Result<Served> {
        let mut lock = self.0.lock.lock().unwrap();
        let known = lock.pins.iter().find(|pin| pin.url == url).cloned();
        let pin = match (known, self.0.mode) {
            (Some(pin), _) if self.0.root.join(&pin.path).is_file() => {
                self.verify(&pin)?;
                pin
            }
            (None, Mode::Offline) => bail!("{url} is not pinned in {LOCK_FILE}"),
            (Some(pin), Mode::Offline) => {
                bail!(
                    "{url} is pinned, but its cache file {} is missing",
                    pin.path
                )
            }
            (known, Mode::Online) => {
                let fetched = self.fetch(url)?;
                match known {
                    Some(pin) => ensure!(
                        pin.sha256 == fetched.sha256,
                        "{url} changed upstream: {LOCK_FILE} pins sha256 {}, the server sent {}",
                        pin.sha256,
                        fetched.sha256
                    ),
                    None => {
                        lock.pins.push(fetched.clone());
                        lock.pins.sort_by(|a, b| a.url.cmp(&b.url));
                        self.save(&lock)?;
                    }
                }
                fetched
            }
        };
        Served::file(&self.0.root.join(&pin.path), &pin.content_type, range)
    }

    fn verify(&self, pin: &Pin) -> Result<()> {
        let sha256 = sha256_of(&self.0.root.join(&pin.path))?;
        ensure!(
            sha256 == pin.sha256,
            "{}: cache file {} has sha256 {sha256}, {LOCK_FILE} pins {}",
            pin.url,
            pin.path,
            pin.sha256
        );
        Ok(())
    }

    fn fetch(&self, url: &str) -> Result<Pin> {
        let cache = self.0.root.join(CACHE_DIR);
        fs::create_dir_all(&cache)?;
        let download = cache.join("download.tmp");
        let output = Command::new("curl")
            .args(["--fail", "--silent", "--show-error", "--location"])
            .args(["--proto", "=http,https", "--proto-redir", "=http,https"])
            .args(["--max-time", FETCH_TIMEOUT_SECONDS])
            .args(["--write-out", "%{content_type}", "--output"])
            .arg(&download)
            .arg(url)
            .output()
            .context("cannot run curl")?;
        ensure!(
            output.status.success(),
            "cannot fetch {url}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        let content_type = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let sha256 = sha256_of(&download)?;
        let path = format!("{CACHE_DIR}/{sha256}");
        fs::rename(&download, self.0.root.join(&path))?;
        Ok(Pin {
            url: url.to_owned(),
            sha256,
            path,
            content_type: if content_type.is_empty() {
                "application/octet-stream".to_owned()
            } else {
                content_type
            },
        })
    }

    fn save(&self, lock: &Lock) -> Result<()> {
        let path = self.0.root.join(LOCK_FILE);
        let staging = path.with_extension("json.tmp");
        let mut text = serde_json::to_string_pretty(lock)?;
        text.push('\n');
        fs::write(&staging, text)?;
        fs::rename(&staging, &path)?;
        Ok(())
    }
}

fn sha256_of(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut context = digest::Context::new(&digest::SHA256);
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        context.update(&chunk[..read]);
    }
    Ok(context
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn url_path(url: &str) -> Option<String> {
    let rest = url.strip_prefix(ORIGIN)?;
    let end = rest.find(['?', '#']).unwrap_or(rest.len());
    let decoded = percent_decode_str(&rest[..end]).decode_utf8().ok()?;
    Some(decoded.trim_start_matches('/').to_owned())
}

fn safe_join(root: &Path, relative: &str) -> Option<PathBuf> {
    let mut path = root.to_owned();
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(part) => path.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(path)
}

fn mime(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    match extension.to_ascii_lowercase().as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "txt" => "text/plain; charset=utf-8",
        "xml" => "application/xml; charset=utf-8",
        "svg" => "image/svg+xml",
        "wasm" => "application/wasm",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "wav" => "audio/wav",
        "ogg" | "oga" => "audio/ogg",
        "flac" => "audio/flac",
        _ => "application/octet-stream",
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Span {
    Whole,
    Part { first: u64, last: u64 },
    Unsatisfiable,
}

fn span(range: Option<&str>, length: u64) -> Span {
    let Some((first, last)) = range
        .and_then(|header| header.trim().strip_prefix("bytes="))
        .and_then(|spec| spec.split_once('-'))
    else {
        return Span::Whole;
    };
    let (first, last) = (first.trim(), last.trim());
    match (first.parse::<u64>(), last.parse::<u64>()) {
        (Ok(first), Ok(last)) if first <= last => clamp(first, last, length),
        (Ok(first), Err(_)) if last.is_empty() => clamp(first, u64::MAX, length),
        (Err(_), Ok(count)) if first.is_empty() => {
            if count == 0 || length == 0 {
                Span::Unsatisfiable
            } else {
                clamp(length.saturating_sub(count), u64::MAX, length)
            }
        }
        _ => Span::Whole,
    }
}

fn clamp(first: u64, last: u64, length: u64) -> Span {
    if first >= length {
        Span::Unsatisfiable
    } else {
        Span::Part {
            first,
            last: last.min(length - 1),
        }
    }
}

struct Served {
    status: i32,
    status_text: &'static str,
    content_type: String,
    headers: Vec<(&'static str, String)>,
    length: i64,
    remaining: u64,
    body: Option<File>,
}

impl Served {
    fn not_found() -> Served {
        Served {
            status: 404,
            status_text: "Not Found",
            content_type: "text/plain; charset=utf-8".to_owned(),
            headers: Vec::new(),
            length: 0,
            remaining: 0,
            body: None,
        }
    }

    fn file(path: &Path, content_type: &str, range: Option<&str>) -> Result<Served> {
        let mut file =
            File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
        let size = file.metadata()?.len();
        let (status, status_text, length, remaining, mut headers) = match span(range, size) {
            Span::Whole => (200, "OK", size as i64, size, Vec::new()),
            Span::Part { first, last } => {
                file.seek(SeekFrom::Start(first))?;
                let content_range = format!("bytes {first}-{last}/{size}");
                let length = if first == 0 { size as i64 } else { -1 };
                (
                    206,
                    "Partial Content",
                    length,
                    last - first + 1,
                    vec![("Content-Range", content_range)],
                )
            }
            Span::Unsatisfiable => (
                416,
                "Range Not Satisfiable",
                -1,
                0,
                vec![("Content-Range", format!("bytes */{size}"))],
            ),
        };
        headers.push(("Content-Length", remaining.to_string()));
        Ok(Served {
            status,
            status_text,
            content_type: content_type.to_owned(),
            headers,
            length,
            remaining,
            body: Some(file),
        })
    }
}

wrap_resource_handler! {
    struct Responder {
        project: Project,
        url: String,
        range: Option<String>,
        served: Arc<Mutex<Option<Served>>>,
    }

    impl ResourceHandler {
        fn open(
            &self,
            _request: Option<&mut Request>,
            handle_request: Option<&mut ::std::os::raw::c_int>,
            _callback: Option<&mut Callback>,
        ) -> ::std::os::raw::c_int {
            if let Some(handle_request) = handle_request {
                *handle_request = 1;
            }
            match self.project.respond(&self.url, self.range.as_deref()) {
                Ok(served) => {
                    *self.served.lock().unwrap() = Some(served);
                    1
                }
                Err(error) => {
                    self.project.fail(format!("{error:#}"));
                    0
                }
            }
        }

        fn response_headers(
            &self,
            response: Option<&mut Response>,
            response_length: Option<&mut i64>,
            _redirect_url: Option<&mut CefString>,
        ) {
            let served = self.served.lock().unwrap();
            let (Some(served), Some(response)) = (served.as_ref(), response) else {
                return;
            };
            response.set_status(served.status);
            response.set_status_text(Some(&served.status_text.into()));
            let mime = served.content_type.split(';').next().unwrap_or_default().trim();
            response.set_mime_type(Some(&mime.into()));
            response.set_header_by_name(
                Some(&"Content-Type".into()),
                Some(&served.content_type.as_str().into()),
                1,
            );
            for (name, value) in [("Accept-Ranges", "bytes"), ("Access-Control-Allow-Origin", "*")] {
                response.set_header_by_name(Some(&name.into()), Some(&value.into()), 1);
            }
            for (name, value) in &served.headers {
                response.set_header_by_name(Some(&(*name).into()), Some(&value.as_str().into()), 1);
            }
            if let Some(response_length) = response_length {
                *response_length = served.length;
            }
        }

        fn skip(
            &self,
            bytes_to_skip: i64,
            bytes_skipped: Option<&mut i64>,
            _callback: Option<&mut ResourceSkipCallback>,
        ) -> ::std::os::raw::c_int {
            // `open` already positioned the file at the range start; CEF still calls skip for open-ended ranges.
            let Some(bytes_skipped) = bytes_skipped else {
                return 0;
            };
            *bytes_skipped = bytes_to_skip;
            1
        }

        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        fn read(
            &self,
            data_out: *mut u8,
            bytes_to_read: ::std::os::raw::c_int,
            bytes_read: Option<&mut ::std::os::raw::c_int>,
            _callback: Option<&mut ResourceReadCallback>,
        ) -> ::std::os::raw::c_int {
            let (Some(bytes_read), true) = (bytes_read, bytes_to_read > 0) else {
                return 0;
            };
            *bytes_read = 0;
            let mut served = self.served.lock().unwrap();
            let Some(served) = served.as_mut() else {
                return 0;
            };
            let Some(file) = served.body.as_mut() else {
                return 0;
            };
            let wanted = served.remaining.min(bytes_to_read as u64) as usize;
            let buffer = unsafe { std::slice::from_raw_parts_mut(data_out, wanted) };
            let read = file.read(buffer).unwrap_or(0);
            served.remaining -= read as u64;
            *bytes_read = read as _;
            (read > 0) as _
        }
    }
}

fn responder(project: &Project, request: &mut Request) -> ResourceHandler {
    let range = CefString::from(&request.header_by_name(Some(&"Range".into()))).to_string();
    Responder::new(
        project.clone(),
        CefString::from(&request.url()).to_string(),
        Some(range).filter(|range| !range.is_empty()),
        Arc::default(),
    )
}

wrap_scheme_handler_factory! {
    struct ProjectFactory {
        project: Project,
    }

    impl SchemeHandlerFactory {
        fn create(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _scheme_name: Option<&CefString>,
            request: Option<&mut Request>,
        ) -> Option<ResourceHandler> {
            request.map(|request| responder(&self.project, request))
        }
    }
}

wrap_resource_request_handler! {
    struct PinnedRequestHandler {
        project: Project,
    }

    impl ResourceRequestHandler {
        fn on_before_resource_load(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _request: Option<&mut Request>,
            _callback: Option<&mut Callback>,
        ) -> ReturnValue {
            ReturnValue::CONTINUE
        }

        fn resource_handler(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            request: Option<&mut Request>,
        ) -> Option<ResourceHandler> {
            request.map(|request| responder(&self.project, request))
        }
    }
}

wrap_request_handler! {
    struct ProjectRequestHandler {
        project: Project,
    }

    impl RequestHandler {
        fn resource_request_handler(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            _is_navigation: ::std::os::raw::c_int,
            _is_download: ::std::os::raw::c_int,
            _request_initiator: Option<&CefString>,
            _disable_default_handling: Option<&mut ::std::os::raw::c_int>,
        ) -> Option<ResourceRequestHandler> {
            let url = CefString::from(&request?.url()).to_string();
            let remote = url.starts_with("http://") || url.starts_with("https://");
            (remote && !self.project.is_direct(&url))
                .then(|| PinnedRequestHandler::new(self.project.clone()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_file_without_a_usable_range() {
        for header in [
            None,
            Some(""),
            Some("items=0-9"),
            Some("bytes=5-2"),
            Some("bytes=a-b"),
            Some("bytes=0-1,4-5"),
        ] {
            assert_eq!(span(header, 100), Span::Whole, "{header:?}");
        }
    }

    #[test]
    fn closed_open_and_suffix_ranges() {
        assert_eq!(
            span(Some("bytes=0-99"), 1000),
            Span::Part { first: 0, last: 99 }
        );
        assert_eq!(
            span(Some("bytes=10-"), 1000),
            Span::Part {
                first: 10,
                last: 999
            }
        );
        assert_eq!(
            span(Some("bytes=-50"), 1000),
            Span::Part {
                first: 950,
                last: 999
            }
        );
        assert_eq!(
            span(Some("bytes=900-5000"), 1000),
            Span::Part {
                first: 900,
                last: 999
            }
        );
        assert_eq!(
            span(Some("bytes=-5000"), 1000),
            Span::Part {
                first: 0,
                last: 999
            }
        );
    }

    #[test]
    fn ranges_past_the_end_are_unsatisfiable() {
        assert_eq!(span(Some("bytes=1000-"), 1000), Span::Unsatisfiable);
        assert_eq!(span(Some("bytes=2000-3000"), 1000), Span::Unsatisfiable);
        assert_eq!(span(Some("bytes=-0"), 1000), Span::Unsatisfiable);
        assert_eq!(span(Some("bytes=0-"), 0), Span::Unsatisfiable);
        assert_eq!(span(Some("bytes=-5"), 0), Span::Unsatisfiable);
    }

    #[test]
    fn paths_stay_inside_the_project() {
        let root = Path::new("/project");
        assert_eq!(
            safe_join(root, "a/b.png"),
            Some(PathBuf::from("/project/a/b.png"))
        );
        assert_eq!(
            safe_join(root, "./a.png"),
            Some(PathBuf::from("/project/a.png"))
        );
        assert_eq!(safe_join(root, ""), Some(PathBuf::from("/project")));
        assert_eq!(safe_join(root, "../secret"), None);
        assert_eq!(safe_join(root, "a/../../secret"), None);
        assert_eq!(
            url_path("vrf://project/%2E%2E/secret").as_deref(),
            Some("../secret")
        );
    }

    #[test]
    fn urls_decode_and_drop_query_and_fragment() {
        assert_eq!(
            url_path("vrf://project/a%20b/c.png?x=1#y").as_deref(),
            Some("a b/c.png")
        );
        assert_eq!(url_path("vrf://project/").as_deref(), Some(""));
        assert_eq!(url_path("vrf://project").as_deref(), Some(""));
        assert_eq!(url_path("https://example.com/a"), None);
    }

    #[test]
    fn media_types_do_not_depend_on_the_machine() {
        assert_eq!(mime(Path::new("a.MP4")), "video/mp4");
        assert_eq!(mime(Path::new("a.woff2")), "font/woff2");
        assert_eq!(mime(Path::new("a.mjs")), "text/javascript; charset=utf-8");
        assert_eq!(mime(Path::new("a")), "application/octet-stream");
    }
}
