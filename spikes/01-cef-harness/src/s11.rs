use crate::{harness::Harness, page_url};
use std::{cell::RefCell, collections::HashMap, rc::Rc, time::{Duration, Instant}};

struct Delivery {
    at: Instant,
    counter: u64,
    probe: [u8; 4],
    fixed: [u8; 4],
}

fn decode(rgba: [u8; 4]) -> Option<u32> {
    (rgba[2] == 165).then(|| rgba[0] as u32 | (rgba[1] as u32) << 8)
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * p) as usize]
}

pub fn run(options: &HashMap<String, String>) {
    let n: u32 = options.get("n").map_or(2000, |v| v.parse().unwrap());
    let pacing = Duration::from_micros((options.get("pacing_ms").map_or(1.0, |v| v.parse().unwrap()) * 1000.0) as u64);
    let damage = options.get("damage").map_or("full", String::as_str).to_string();
    let watchdog = Duration::from_millis(options.get("watchdog_ms").map_or(250, |v| v.parse().unwrap()));
    let (width, height) = (1280, 720);
    let probe = if damage == "small" { (32, 32) } else { (width as u32 / 2, height as u32 / 2) };

    let h = Harness::open(&page_url(&format!("s11.html?{damage}")), width, height);
    let log: Rc<RefCell<Vec<Delivery>>> = Rc::default();
    {
        let (log, shared) = (log.clone(), h.shared.clone());
        h.set_paint(move |info| {
            let frame = shared.gpu.import_frame(info);
            let origin = (info.extra.visible_rect.x as u32, info.extra.visible_rect.y as u32);
            let px = shared.gpu.read_points(&frame, origin, &[probe, (1000, 600)]);
            log.borrow_mut().push(Delivery {
                at: Instant::now(),
                counter: info.extra.capture_counter,
                probe: px[0],
                fixed: px[1],
            });
        });
    }

    h.eval("seek(65535)");
    for _ in 0..3 {
        h.begin_frame();
        h.pump_for(Duration::from_millis(100));
    }
    log.borrow_mut().clear();

    if options.contains_key("nodamage") {
        let mut delivered = 0;
        for _ in 0..n {
            let before = log.borrow().len();
            h.begin_frame();
            if h.pump_until(watchdog, || log.borrow().len() > before) {
                delivered += 1;
            }
        }
        println!("spike=1.1 nodamage begin_frames_sent={n} deliveries_within_{}ms={delivered} total_deliveries={} load={}", watchdog.as_millis(), log.borrow().len(), crate::load_average());
        h.close();
        return;
    }

    let (mut exact, mut stale, mut ahead, mut garbage, mut dropped) = (0u32, 0u32, 0u32, 0u32, 0u32);
    let mut latencies = Vec::new();
    let mut anomalies = Vec::new();
    let mut last_bf = Instant::now();
    let started = Instant::now();
    for i in 0..n {
        h.pump_until(Duration::from_secs(5), || last_bf.elapsed() >= pacing);
        h.eval(&format!("seek({i})"));
        let start = log.borrow().len();
        let sent = Instant::now();
        last_bf = sent;
        h.begin_frame();
        let got = h.pump_until(watchdog, || log.borrow().len() > start);
        if !got {
            dropped += 1;
            anomalies.push(format!("frame {i}: no delivery within {watchdog:?}"));
            continue;
        }
        let log = log.borrow();
        let d = &log[start];
        latencies.push((d.at - sent).as_secs_f64() * 1000.0);
        match decode(d.probe) {
            Some(idx) if idx == i => exact += 1,
            Some(idx) if idx < i => {
                stale += 1;
                anomalies.push(format!("frame {i}: stale, delivery shows {idx} (counter {})", d.counter));
            }
            Some(idx) => {
                ahead += 1;
                anomalies.push(format!("frame {i}: ahead, delivery shows {idx} (counter {})", d.counter));
            }
            None => {
                garbage += 1;
                anomalies.push(format!("frame {i}: undecodable pixel {:?} (counter {})", d.probe, d.counter));
            }
        }
    }
    let elapsed = started.elapsed();
    h.pump_for(Duration::from_millis(500));

    let log = log.borrow();
    let extra = log.len() as i64 - (n - dropped) as i64;
    let repeated = log.windows(2).filter(|w| decode(w[0].probe).is_some() && decode(w[0].probe) == decode(w[1].probe)).count();
    let gaps = log.windows(2).filter(|w| w[1].counter != w[0].counter + 1).count();
    let fixed_bad = if damage == "small" { log.iter().filter(|d| d.fixed != [255, 255, 255, 255]).count() } else { 0 };
    latencies.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("spike=1.1 damage={damage} pacing_ms={} frames={n} raster={} load={}", pacing.as_secs_f64() * 1000.0, crate::harness::raster_mode(), crate::load_average());
    println!("begin_frames_sent={n} deliveries={} extra_deliveries={extra} repeated_pixels={repeated}", log.len());
    println!("exact={exact} stale={stale} ahead={ahead} undecodable={garbage} dropped={dropped} mismatches={}", stale + ahead + garbage);
    println!("capture_counter first={} last={} gaps={gaps}", log.first().map_or(0, |d| d.counter), log.last().map_or(0, |d| d.counter));
    println!("static_pixel_wrong={fixed_bad}");
    if !latencies.is_empty() {
        println!("delivery_latency_ms p50={:.2} p99={:.2} max={:.2}", percentile(&latencies, 0.5), percentile(&latencies, 0.99), latencies.last().unwrap());
    }
    println!("wall_s={:.2} frames_per_s={:.1}", elapsed.as_secs_f64(), n as f64 / elapsed.as_secs_f64());
    for line in anomalies.iter().take(12) {
        println!("  {line}");
    }
    h.close();
}
