use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use vrf::sched::{Submission, ordered};

struct Xorshift(u64);

impl Xorshift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::yield_now();
    }
}

#[test]
fn four_producers_with_random_delays_arrive_in_order() {
    const FRAMES: u64 = 1000;
    const PRODUCERS: u64 = 4;
    let (producer, mut consumer) = ordered::<Arc<u64>>(16);
    let handles: Vec<_> = (0..PRODUCERS)
        .map(|p| {
            let producer = producer.clone();
            thread::spawn(move || {
                let mut rng = Xorshift(0x9e37_79b9_7f4a_7c15 ^ (p + 1));
                for index in (p..FRAMES).step_by(PRODUCERS as usize) {
                    thread::sleep(Duration::from_micros(rng.next() % 2000));
                    producer
                        .submit(index, Submission::Frame(Arc::new(index)))
                        .unwrap();
                }
            })
        })
        .collect();
    drop(producer);

    let mut seen = 0;
    while let Some((index, frame)) = consumer.recv().unwrap() {
        assert_eq!(index, seen);
        assert_eq!(*frame, seen);
        seen += 1;
    }
    assert_eq!(seen, FRAMES);
    for handle in handles {
        handle.join().unwrap();
    }
    let stats = consumer.stats();
    assert!(stats.high_water <= 16, "buffered {}", stats.high_water);
    assert_eq!(stats.buffered, 0);
}

#[test]
fn producer_a_pool_ahead_blocks_until_the_gap_closes() {
    const POOL: usize = 64;
    let (producer, mut consumer) = ordered::<u64>(POOL);
    let returned = Arc::new(AtomicBool::new(false));
    let ahead = {
        let producer = producer.clone();
        let returned = returned.clone();
        thread::spawn(move || {
            producer
                .submit(POOL as u64, Submission::Frame(POOL as u64))
                .unwrap();
            returned.store(true, Ordering::SeqCst);
        })
    };

    wait_until("the far-ahead producer to block", || {
        consumer.stats().blocked == 1
    });
    assert!(!returned.load(Ordering::SeqCst));
    assert_eq!(consumer.stats().buffered, 0);

    for index in 1..POOL as u64 {
        producer.submit(index, Submission::Frame(index)).unwrap();
    }
    assert_eq!(consumer.stats().buffered, POOL - 1);
    assert_eq!(consumer.stats().blocked, 1);
    assert!(!returned.load(Ordering::SeqCst));

    producer.submit(0, Submission::Frame(0)).unwrap();
    assert_eq!(consumer.recv().unwrap(), Some((0, 0)));
    ahead.join().unwrap();
    assert!(returned.load(Ordering::SeqCst));

    drop(producer);
    for index in 1..=POOL as u64 {
        assert_eq!(consumer.recv().unwrap(), Some((index, index)));
    }
    assert_eq!(consumer.recv().unwrap(), None);
    assert_eq!(consumer.stats().high_water, POOL);
}

#[test]
fn a_frame_inside_the_window_does_not_block() {
    let (producer, consumer) = ordered::<u8>(4);
    producer.submit(3, Submission::Frame(3)).unwrap();
    assert_eq!(consumer.stats().blocked, 0);
    assert_eq!(consumer.stats().buffered, 1);
}

#[test]
fn repeat_matches_submitting_the_same_frame_twice() {
    let a: Arc<[u8]> = Arc::from(vec![1u8, 2, 3, 4]);
    let b: Arc<[u8]> = Arc::from(vec![9u8, 8, 7, 6]);

    let run = |second: Submission<Arc<[u8]>>| {
        let (producer, mut consumer) = ordered::<Arc<[u8]>>(8);
        producer.submit(2, Submission::Frame(b.clone())).unwrap();
        producer.submit(1, second).unwrap();
        producer.submit(0, Submission::Frame(a.clone())).unwrap();
        drop(producer);
        let mut out = Vec::new();
        while let Some(item) = consumer.recv().unwrap() {
            out.push(item);
        }
        out
    };

    let repeated = run(Submission::Repeat);
    let doubled = run(Submission::Frame(a.clone()));
    assert_eq!(repeated, doubled);
    assert_eq!(repeated.len(), 3);
    assert!(Arc::ptr_eq(&repeated[0].1, &repeated[1].1));
    assert_eq!(&*repeated[1].1, &[1, 2, 3, 4]);
}

#[test]
fn repeats_chain_through_consecutive_indices() {
    let (producer, mut consumer) = ordered::<u32>(8);
    producer.submit(3, Submission::Repeat).unwrap();
    producer.submit(2, Submission::Repeat).unwrap();
    producer.submit(1, Submission::Repeat).unwrap();
    producer.submit(0, Submission::Frame(7)).unwrap();
    drop(producer);
    let mut out = Vec::new();
    while let Some((_, frame)) = consumer.recv().unwrap() {
        out.push(frame);
    }
    assert_eq!(out, [7, 7, 7, 7]);
}

#[test]
fn repeating_index_zero_is_rejected() {
    let (producer, _consumer) = ordered::<u8>(4);
    assert!(producer.submit(0, Submission::Repeat).is_err());
}

#[test]
fn duplicate_and_stale_indices_are_rejected() {
    let (producer, mut consumer) = ordered::<u8>(4);
    producer.submit(0, Submission::Frame(0)).unwrap();
    assert!(producer.submit(0, Submission::Frame(1)).is_err());
    assert_eq!(consumer.recv().unwrap(), Some((0, 0)));
    assert!(producer.submit(0, Submission::Frame(2)).is_err());
}

#[test]
fn a_missing_frame_is_an_error_not_a_hang() {
    let (producer, mut consumer) = ordered::<u8>(4);
    producer.submit(1, Submission::Frame(1)).unwrap();
    drop(producer);
    let err = consumer.recv().unwrap_err().to_string();
    assert!(err.contains("frame 0"), "{err}");
}

#[test]
fn dropping_the_consumer_unblocks_producers() {
    let (producer, consumer) = ordered::<u8>(2);
    let blocked = {
        let producer = producer.clone();
        thread::spawn(move || producer.submit(2, Submission::Frame(2)))
    };
    wait_until("the producer to block", || consumer.stats().blocked == 1);
    drop(consumer);
    assert!(blocked.join().unwrap().is_err());
}
