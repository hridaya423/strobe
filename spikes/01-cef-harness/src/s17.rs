use crate::{harness::Harness, page_url};
use std::{cell::RefCell, collections::HashMap, rc::Rc, time::{Duration, Instant}};

struct Run {
    label: String,
    hashes: Vec<[u8; 32]>,
    kept: HashMap<usize, Vec<u8>>,
    log: Vec<[f64; 4]>,
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    values[values.len() / 2]
}

fn one_run(label: &str, page: &str, pacing: Duration, n: usize, keep: &[usize]) -> Run {
    let (width, height) = (1280u32, 720u32);
    let h = Harness::open(&page_url(page), width as i32, height as i32);
    let hashes: Rc<RefCell<Vec<[u8; 32]>>> = Rc::default();
    let kept: Rc<RefCell<HashMap<usize, Vec<u8>>>> = Rc::default();
    {
        let (hashes, kept, shared, keep) = (hashes.clone(), kept.clone(), h.shared.clone(), keep.to_vec());
        h.set_paint(move |info| {
            let frame = shared.gpu.import_frame(info);
            let origin = (info.extra.visible_rect.x as u32, info.extra.visible_rect.y as u32);
            let pixels = shared.gpu.read_frame(&frame, origin, (width, height));
            let index = hashes.borrow().len();
            hashes.borrow_mut().push(*blake3::hash(&pixels).as_bytes());
            if keep.contains(&index) {
                kept.borrow_mut().insert(index, pixels);
            }
        });
    }
    for _ in 0..3 {
        h.begin_frame();
        h.pump_for(Duration::from_millis(100));
    }
    hashes.borrow_mut().clear();
    let mut last = Instant::now();
    for _ in 0..n {
        h.pump_until(Duration::from_secs(5), || last.elapsed() >= pacing);
        let before = hashes.borrow().len();
        last = Instant::now();
        h.begin_frame();
        h.pump_until(Duration::from_millis(500), || hashes.borrow().len() > before);
    }
    h.pump_for(Duration::from_millis(200));
    let log: Vec<[f64; 4]> = serde_json::from_str(h.eval("JSON.stringify(log)").as_str().unwrap()).unwrap_or_default();
    h.close();
    let hashes = hashes.borrow().clone();
    let kept = kept.borrow().clone();
    Run { label: label.into(), hashes, kept, log }
}

fn compare(a: &Run, b: &Run, keep: &[usize]) {
    let n = a.hashes.len().min(b.hashes.len());
    let same = (0..n).filter(|&i| a.hashes[i] == b.hashes[i]).count();
    println!("  {} vs {}: identical frames {same}/{n} (deliveries {} vs {})", a.label, b.label, a.hashes.len(), b.hashes.len());
    for &i in keep {
        if let (Some(x), Some(y)) = (a.kept.get(&i), b.kept.get(&i)) {
            let differing = x.chunks_exact(4).zip(y.chunks_exact(4)).filter(|(p, q)| p != q).count();
            let max_delta = x.iter().zip(y).map(|(p, q)| p.abs_diff(*q)).max().unwrap();
            println!("    frame {i}: differing pixels {differing}, max channel delta {max_delta}");
        }
    }
}

pub fn run(options: &HashMap<String, String>) {
    let n: usize = options.get("n").map_or(200, |v| v.parse().unwrap());
    let keep = [20usize, 60];
    let only = options.get("only").map_or("all", String::as_str);
    let page = format!("s17.html?{only}");
    println!("spike=1.7 content={only} frames={n} raster={} load={}", crate::harness::raster_mode(), crate::load_average());
    let runs = [
        one_run("1ms-a", &page, Duration::from_millis(1), n, &keep),
        one_run("50ms", &page, Duration::from_millis(50), n, &keep),
        one_run("1ms-b", &page, Duration::from_millis(1), n, &keep),
    ];
    compare(&runs[0], &runs[1], &keep);
    compare(&runs[0], &runs[2], &keep);
    for r in runs.iter().filter(|r| !r.log.is_empty()) {
        let tail = &r.log[r.log.len().saturating_sub(n)..];
        let deltas = |k: usize| median(tail.windows(2).map(|w| w[1][k] - w[0][k]).collect());
        println!(
            "  {}: page-visible clocks, median delta per frame: rAF timestamp {:.2} ms, performance.now {:.2} ms, document.timeline {:.2} ms, Date.now {:.2} ms; first rAF timestamp {:.1}",
            r.label, deltas(0), deltas(1), deltas(2), deltas(3), r.log[0][0]
        );
    }
}
