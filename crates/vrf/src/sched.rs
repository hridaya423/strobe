use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use anyhow::{Result, bail, ensure};

/// What a producer hands in for one frame index.
pub enum Submission<F> {
    Frame(F),
    /// Re-submits the frame released just before this index, so the consumer sees a normal frame.
    Repeat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub buffered: usize,
    pub high_water: usize,
    /// Producers currently waiting for the window to reach their index.
    pub blocked: usize,
}

struct State<F> {
    next: u64,
    slots: Vec<Option<Submission<F>>>,
    last: Option<F>,
    producers: usize,
    consumer_gone: bool,
    stats: Stats,
}

struct Shared<F> {
    state: Mutex<State<F>>,
    space: Condvar,
    ready: Condvar,
}

impl<F> Shared<F> {
    fn lock(&self) -> MutexGuard<'_, State<F>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Creates a reorder buffer that holds at most `capacity` frames.
///
/// Index `i` is accepted while `i < next + capacity`, where `next` is the lowest index not yet
/// released. A producer further ahead blocks in [`Producer::submit`].
pub fn ordered<F: Clone>(capacity: usize) -> (Producer<F>, Consumer<F>) {
    assert!(capacity > 0, "capacity must be positive");
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            next: 0,
            slots: (0..capacity).map(|_| None).collect(),
            last: None,
            producers: 1,
            consumer_gone: false,
            stats: Stats {
                buffered: 0,
                high_water: 0,
                blocked: 0,
            },
        }),
        space: Condvar::new(),
        ready: Condvar::new(),
    });
    (
        Producer {
            shared: shared.clone(),
        },
        Consumer { shared },
    )
}

pub struct Producer<F> {
    shared: Arc<Shared<F>>,
}

impl<F> Producer<F> {
    /// Blocks while `index` is a full window ahead of the next frame to release.
    pub fn submit(&self, index: u64, submission: Submission<F>) -> Result<()> {
        ensure!(
            index > 0 || matches!(submission, Submission::Frame(_)),
            "frame 0 cannot repeat a previous frame"
        );
        let mut state = self.shared.lock();
        let capacity = state.slots.len() as u64;
        ensure!(
            index >= state.next,
            "frame {index} was already released (next is {})",
            state.next
        );
        if index >= state.next + capacity {
            state.stats.blocked += 1;
            while index >= state.next + capacity && !state.consumer_gone {
                state = self
                    .shared
                    .space
                    .wait(state)
                    .unwrap_or_else(|e| e.into_inner());
            }
            state.stats.blocked -= 1;
        }
        if state.consumer_gone {
            bail!("the consumer is gone");
        }
        ensure!(
            index >= state.next,
            "frame {index} was already released (next is {})",
            state.next
        );
        let slot = (index % capacity) as usize;
        ensure!(
            state.slots[slot].is_none(),
            "frame {index} was submitted twice"
        );
        state.slots[slot] = Some(submission);
        state.stats.buffered += 1;
        state.stats.high_water = state.stats.high_water.max(state.stats.buffered);
        if index == state.next {
            self.shared.ready.notify_one();
        }
        Ok(())
    }
}

impl<F> Clone for Producer<F> {
    fn clone(&self) -> Self {
        self.shared.lock().producers += 1;
        Producer {
            shared: self.shared.clone(),
        }
    }
}

impl<F> Drop for Producer<F> {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        state.producers -= 1;
        if state.producers == 0 {
            self.shared.ready.notify_one();
        }
    }
}

pub struct Consumer<F> {
    shared: Arc<Shared<F>>,
}

impl<F: Clone> Consumer<F> {
    /// Returns the next frame in index order, or `None` once every producer is gone and the
    /// buffer is empty. Fails if producers are gone but a later frame is buffered behind a gap.
    pub fn recv(&mut self) -> Result<Option<(u64, F)>> {
        let mut state = self.shared.lock();
        loop {
            let capacity = state.slots.len() as u64;
            let index = state.next;
            if let Some(submission) = state.slots[(index % capacity) as usize].take() {
                let frame = match submission {
                    Submission::Frame(frame) => frame,
                    Submission::Repeat => state.last.clone().expect("index 0 is never a repeat"),
                };
                state.last = Some(frame.clone());
                state.next += 1;
                state.stats.buffered -= 1;
                self.shared.space.notify_all();
                return Ok(Some((index, frame)));
            }
            if state.producers == 0 {
                if state.stats.buffered == 0 {
                    return Ok(None);
                }
                bail!(
                    "frame {index} never arrived; {} later frames are stuck behind it",
                    state.stats.buffered
                );
            }
            state = self
                .shared
                .ready
                .wait(state)
                .unwrap_or_else(|e| e.into_inner());
        }
    }
}

impl<F> Consumer<F> {
    pub fn stats(&self) -> Stats {
        self.shared.lock().stats
    }
}

impl<F> Drop for Consumer<F> {
    fn drop(&mut self) {
        self.shared.lock().consumer_gone = true;
        self.shared.space.notify_all();
    }
}
