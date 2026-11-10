use std::collections::BTreeMap;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
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
    frames: usize,
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
        Stream::start(clip.pts.len(), move |shared| decode(shared, &clip))
    }

    fn start(frames: usize, work: impl FnOnce(&Shared) -> Result<()> + Send + 'static) -> Stream {
        let shared = Arc::new(Shared {
            frames,
            state: Mutex::default(),
            changed: Condvar::new(),
        });
        let worker = thread::spawn({
            let shared = shared.clone();
            move || {
                let failure = match panic::catch_unwind(AssertUnwindSafe(|| work(&shared))) {
                    Ok(Ok(())) => None,
                    Ok(Err(error)) => Some(format!("{error:#}")),
                    Err(payload) => Some(panic_text(payload.as_ref())),
                };
                if let Some(failure) = failure {
                    shared.lock().error = Some(failure);
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
        let (low, high) = (from, (from + ahead).min(self.shared.frames - 1));
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
        self.wait(index)
    }

    /// Waits for frame `index`, which must lie in the window the last [`Stream::want`] set.
    pub fn wait(&self, index: usize) -> Result<Arc<Frame>> {
        let mut state = self.shared.state.lock().unwrap();
        loop {
            if let Some(frame) = state.cache.get(&index) {
                return Ok(frame.clone());
            }
            if let Some(error) = &state.error {
                bail!("{error}");
            }
            if state.closed {
                bail!("the stream was closed");
            }
            state = self.shared.changed.wait(state).unwrap();
        }
    }

    /// Waits for frame `index` while it lies in the window, and gives `None` once it has left the
    /// window or the stream is closed.
    pub fn wait_wanted(&self, index: usize) -> Result<Option<Arc<Frame>>> {
        let mut state = self.shared.state.lock().unwrap();
        loop {
            if let Some(frame) = state.cache.get(&index) {
                return Ok(Some(frame.clone()));
            }
            if let Some(error) = &state.error {
                bail!("{error}");
            }
            if state.closed || index < state.low || index > state.high {
                return Ok(None);
            }
            state = self.shared.changed.wait(state).unwrap();
        }
    }

    /// Stops the decoder and wakes every waiter.
    pub fn close(&self) {
        self.shared.state.lock().unwrap().closed = true;
        self.shared.changed.notify_all();
    }

    /// Frames decoded and reader restarts so far.
    pub fn counters(&self) -> (u64, u64) {
        let state = self.shared.state.lock().unwrap();
        (state.decoded, state.restarts)
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.close();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    let text = payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_owned());
    format!("the video decoder thread panicked: {text}")
}

fn decode(shared: &Shared, clip: &Clip) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_in_the_worker_fails_the_waiter_instead_of_hanging() {
        let stream = Stream::start(10, |_| panic!("decoder exploded"));
        let error = stream.get(3, 2).err().expect("the wait should fail");
        assert!(
            format!("{error:#}").contains("decoder exploded"),
            "{error:#}"
        );
        let error = stream.wait(5).err().expect("later waits fail too");
        assert!(
            format!("{error:#}").contains("decoder exploded"),
            "{error:#}"
        );
    }

    #[test]
    fn waiting_on_a_closed_stream_is_an_error() {
        let stream = Stream::start(10, |shared| {
            let mut state = shared.lock();
            while !state.closed {
                state = shared.changed.wait(state).unwrap();
            }
            Ok(())
        });
        stream.want(0, 2);
        stream.close();
        let error = stream.wait(1).err().expect("the wait should fail");
        assert!(format!("{error:#}").contains("closed"), "{error:#}");
    }
}
