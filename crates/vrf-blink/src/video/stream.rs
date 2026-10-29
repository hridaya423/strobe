use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use anyhow::{Result, anyhow, bail};

use super::decoder::{Clip, Frame, Reader};

const KEEP_BEHIND: usize = 2;
const SKIP_FORWARD: usize = 48;

#[derive(Default)]
struct State {
    low: usize,
    high: usize,
    cache: BTreeMap<usize, Arc<Frame>>,
    error: Option<String>,
    closed: bool,
    restarts: u64,
    decoded: u64,
}

impl State {
    fn first_missing(&self) -> Option<usize> {
        (self.low..=self.high).find(|index| !self.cache.contains_key(index))
    }
}

struct Shared {
    clip: Arc<Clip>,
    state: Mutex<State>,
    changed: Condvar,
}

/// A decoder thread for one clip in one view. It keeps the frames from the one asked for to a few
/// ahead decoded in memory, decoding forward from where it is while that is cheaper than a
/// restart and restarting the reader at the first missing frame otherwise. A frame is only ever
/// handed out under the index it was decoded for, so a jump in either direction costs time and
/// never shows the wrong picture.
pub struct Stream {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

impl Stream {
    pub fn new(clip: Arc<Clip>) -> Stream {
        let shared = Arc::new(Shared {
            clip,
            state: Mutex::default(),
            changed: Condvar::new(),
        });
        let worker = thread::spawn({
            let shared = shared.clone();
            move || {
                if let Err(error) = decode(&shared) {
                    shared.state.lock().unwrap().error = Some(format!("{error:#}"));
                    shared.changed.notify_all();
                }
            }
        });
        Stream {
            shared,
            worker: Some(worker),
        }
    }

    /// Asks for frames `from..=from + ahead` to be decoded, and returns without waiting.
    pub fn want(&self, from: usize, ahead: usize) {
        let mut state = self.shared.state.lock().unwrap();
        let (low, high) = (from, (from + ahead).min(self.shared.clip.pts.len() - 1));
        state.low = low;
        state.high = high;
        state
            .cache
            .retain(|index, _| *index + KEEP_BEHIND >= low && *index <= high);
        self.shared.changed.notify_all();
    }

    /// Waits for frame `index`, with the following `ahead` frames queued behind it.
    pub fn get(&self, index: usize, ahead: usize) -> Result<Arc<Frame>> {
        self.want(index, ahead);
        let mut state = self.shared.state.lock().unwrap();
        loop {
            if let Some(frame) = state.cache.get(&index) {
                return Ok(frame.clone());
            }
            if let Some(error) = &state.error {
                bail!("{error}");
            }
            state = self.shared.changed.wait(state).unwrap();
        }
    }

    /// The frame if it is already decoded.
    pub fn peek(&self, index: usize) -> Option<Arc<Frame>> {
        self.shared.state.lock().unwrap().cache.get(&index).cloned()
    }

    /// Frames decoded and reader restarts so far.
    pub fn counters(&self) -> (u64, u64) {
        let state = self.shared.state.lock().unwrap();
        (state.decoded, state.restarts)
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.shared.state.lock().unwrap().closed = true;
        self.shared.changed.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn decode(shared: &Shared) -> Result<()> {
    let clip = &shared.clip;
    let mut reader: Option<(Reader, usize)> = None;
    loop {
        let missing = {
            let mut state = shared.state.lock().unwrap();
            loop {
                if state.closed {
                    return Ok(());
                }
                if let Some(missing) = state.first_missing() {
                    break missing;
                }
                state = shared.changed.wait(state).unwrap();
            }
        };
        let usable = reader
            .as_ref()
            .is_some_and(|(_, next)| *next <= missing && missing - *next <= SKIP_FORWARD);
        if !usable {
            reader = Some((Reader::open(clip, missing)?, missing));
            shared.state.lock().unwrap().restarts += 1;
        }
        let (stream, next) = reader.as_mut().expect("a reader was just opened");
        let (index, frame) = stream
            .next(clip)?
            .ok_or_else(|| anyhow!("the stream ended before frame {missing}"))?;
        *next = index + 1;
        let mut state = shared.state.lock().unwrap();
        state.decoded += 1;
        if index >= state.low && index <= state.high {
            state.cache.insert(index, Arc::new(frame));
            shared.changed.notify_all();
        }
    }
}
