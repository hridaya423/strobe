use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result};
use cef::*;
use serde::Deserialize;
use serde_json::json;

use super::PUSH;
use super::decoder::{Clip, Frame, Matrix};
use super::stream::Stream;
use crate::app::{OnUi, Ui};
use crate::{Project, View};

const HEADER: usize = 32;
const FRAME: u32 = 1;
const META: u32 = 2;
const FAILURE: u32 = 3;
const LOOK_AHEAD: usize = 3;
const MAX_STRIDE: usize = 8;
const KEEP_BEHIND: usize = 2;
const KEEP_AHEAD: usize = 12;

/// Sends shared-memory messages to the main frame of one view's page.
#[derive(Clone)]
pub struct Pusher {
    ui: Ui,
    host: OnUi<BrowserHost>,
}

impl Pusher {
    pub(crate) fn new(ui: Ui, host: OnUi<BrowserHost>) -> Pusher {
        Pusher { ui, host }
    }

    fn send(&self, len: usize, fill: impl FnOnce(&mut [u8])) -> Result<()> {
        let builder = shared_process_message_builder_create(Some(&PUSH.into()), len)
            .context("no shared message builder")?;
        fill(unsafe { std::slice::from_raw_parts_mut(builder.memory().cast::<u8>(), len) });
        let message = OnUi(builder.build().context("no shared message")?);
        let host = self.host.clone();
        self.ui.post(move || {
            let mut message = message.0;
            if let Some(frame) = host.0.browser().and_then(|browser| browser.main_frame()) {
                frame.send_process_message(ProcessId::RENDERER, Some(&mut message));
            }
        });
        Ok(())
    }

    fn header(buffer: &mut [u8], kind: u32, clip: u32, a: u32, b: u32, c: u32) {
        for (slot, value) in [kind, clip, a, b, c].into_iter().enumerate() {
            buffer[slot * 4..slot * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
    }

    fn frame(&self, clip: u32, index: usize, width: u32, height: u32, frame: &Frame) -> Result<()> {
        let len = Frame::nv12_len(width, height);
        self.send(HEADER + len, |buffer| {
            Self::header(buffer, FRAME, clip, index as u32, width, height);
            frame.write_nv12(&mut buffer[HEADER..]);
        })
    }

    fn text(
        &self,
        kind: u32,
        clip: u32,
        count: u32,
        k: u32,
        json: &serde_json::Value,
        table: &[i64],
    ) -> Result<()> {
        let text = json.to_string();
        let padded = text.len().next_multiple_of(8);
        self.send(HEADER + padded + table.len() * 8, |buffer| {
            Self::header(buffer, kind, clip, count, text.len() as u32, k);
            buffer[HEADER..HEADER + text.len()].copy_from_slice(text.as_bytes());
            for (slot, pts) in table.iter().enumerate() {
                let at = HEADER + padded + slot * 8;
                buffer[at..at + 8].copy_from_slice(&(*pts as f64).to_le_bytes());
            }
        })
    }
}

/// The clips opened so far, shared by every view of one render.
#[derive(Default)]
pub struct Clips {
    opened: Mutex<HashMap<PathBuf, Arc<Clip>>>,
}

impl Clips {
    fn open(&self, path: &Path) -> Result<Arc<Clip>> {
        let mut opened = self.opened.lock().unwrap();
        if let Some(clip) = opened.get(path) {
            return Ok(clip.clone());
        }
        let clip = Arc::new(Clip::open(path)?);
        opened.insert(path.to_owned(), clip.clone());
        Ok(clip)
    }
}

/// Starts the server for clips the NV12 path refuses, once, and returns its port and the URL it
/// serves the bundle from.
pub type StartFallback = Arc<dyn Fn() -> Result<(u16, String)> + Send + Sync>;

#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
enum Message {
    Open { req: u32, src: String },
    Want { clip: u32, k: usize, hit: bool },
}

struct Playing {
    clip: Arc<Clip>,
    stream: Stream,
    pushed: BTreeSet<usize>,
    last: Option<usize>,
}

/// Serves one view's page: opens the clips it asks for, decodes ahead of what it shows, and
/// pushes the frames it will want next, so that in steady state the page finds each frame already
/// in memory and the host is never on the page's critical path.
pub struct Link;

impl Link {
    /// Starts the thread. It ends when the view closes.
    pub fn attach(view: &mut View, project: Project, clips: Arc<Clips>, fallback: StartFallback) {
        let pusher = view.pusher();
        let messages = view
            .take_pages()
            .expect("the view's page messages were already taken");
        thread::spawn(move || serve(messages, pusher, project, clips, fallback));
    }
}

fn serve(
    messages: Receiver<String>,
    pusher: Pusher,
    project: Project,
    clips: Arc<Clips>,
    fallback: StartFallback,
) {
    let mut playing: Vec<Playing> = Vec::new();
    while let Ok(text) = messages.recv() {
        let result = serde_json::from_str::<Message>(&text)
            .context("the page sent a message the host does not know")
            .and_then(|message| match message {
                Message::Open { req, src } => open(
                    &pusher,
                    &project,
                    &clips,
                    &fallback,
                    &mut playing,
                    req,
                    &src,
                ),
                Message::Want { clip, k, hit } => match playing.get_mut(clip as usize) {
                    Some(playing) => want(&pusher, clip, playing, k, hit),
                    None => Ok(()),
                },
            });
        if let Err(error) = result {
            eprintln!("video: {error:#}");
        }
    }
}

fn open(
    pusher: &Pusher,
    project: &Project,
    clips: &Clips,
    fallback: &StartFallback,
    playing: &mut Vec<Playing>,
    req: u32,
    src: &str,
) -> Result<()> {
    let id = playing.len() as u32;
    let clip = project
        .file_for(src)
        .context("not a file in the project")
        .and_then(|path| clips.open(&path))
        .map_err(|error| format!("{error:#}"))
        .and_then(|clip| clip.refusal().map_or(Ok(clip), Err));
    let clip = match clip {
        Ok(clip) => clip,
        Err(reason) => {
            let meta = match fallback() {
                Ok((port, serve_url)) => {
                    json!({"req": req, "eligible": false, "reason": reason, "fallback": {"port": port, "serveUrl": serve_url}})
                }
                Err(error) => {
                    json!({"req": req, "eligible": false, "reason": reason, "fallbackError": format!("{error:#}")})
                }
            };
            return pusher.text(META, id, 0, 0, &meta, &[]);
        }
    };
    let meta = json!({
        "req": req,
        "eligible": true,
        "width": clip.width,
        "height": clip.height,
        "timescale": clip.timescale,
        "matrix": match clip.matrix { Matrix::Bt709 => "709", Matrix::Bt601 => "601" },
        "full": clip.full_range,
    });
    pusher.text(META, id, clip.pts.len() as u32, 0, &meta, &clip.pts)?;
    playing.push(Playing {
        stream: Stream::new(clip.clone()),
        clip,
        pushed: BTreeSet::new(),
        last: None,
    });
    Ok(())
}

fn want(pusher: &Pusher, id: u32, playing: &mut Playing, k: usize, hit: bool) -> Result<()> {
    let count = playing.clip.pts.len();
    if k >= count {
        return Ok(());
    }
    let stride = match playing.last {
        Some(last) if k > last && k - last <= MAX_STRIDE => k - last,
        _ => 1,
    };
    playing.last = Some(k);
    if !hit {
        playing.pushed.remove(&k);
    }
    playing.stream.want(k, LOOK_AHEAD * stride);
    let wanted = (0..=LOOK_AHEAD)
        .map(|step| k + step * stride)
        .take_while(|index| *index < count);
    for index in wanted {
        if playing.pushed.contains(&index) {
            continue;
        }
        match playing.stream.wait(index) {
            Ok(frame) => {
                pusher.frame(id, index, playing.clip.width, playing.clip.height, &frame)?;
                playing.pushed.insert(index);
            }
            Err(error) => {
                let failure = json!({"error": format!("{error:#}")});
                return pusher.text(FAILURE, id, 0, index as u32, &failure, &[]);
            }
        }
    }
    playing
        .pushed
        .retain(|index| *index + KEEP_BEHIND >= k && *index <= k + KEEP_AHEAD);
    Ok(())
}
