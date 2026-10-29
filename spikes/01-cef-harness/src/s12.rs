use crate::{harness::Harness, page_url};
use std::{cell::RefCell, collections::HashMap, rc::Rc, time::Duration};

type Point = (u32, u32);

struct Delivery {
    counter: u64,
    pixels: Vec<[u8; 4]>,
}

fn expected(variant: &str, i: u32) -> Vec<(Point, [u8; 3])> {
    match variant {
        "layout" => {
            let w = 100 + (i * 37) % 900;
            vec![
                ((w - 10, 360), [255, 0, 0]),
                ((w + 100, 360), [0, 255, 0]),
                ((w + 205, 360), [0, 0, 255]),
            ]
        }
        "transform" => vec![(((i * 173) % 1000 + 50, 350), [255, 0, 0])],
        _ => vec![((32, 32), [(i & 255) as u8, ((i >> 8) & 255) as u8, 165])],
    }
}

fn prime_points(variant: &str, i: u32) -> Vec<Point> {
    expected(variant, i).iter().map(|(p, _)| *p).collect()
}

fn matches(variant: &str, i: u32, pixels: &[[u8; 4]], offset: usize) -> bool {
    expected(variant, i)
        .iter()
        .enumerate()
        .all(|(k, (_, rgb))| pixels[offset + k][..3] == rgb[..])
}

#[derive(Default, Clone, Copy)]
struct Tally {
    correct: u32,
    stale: u32,
    none: u32,
    other: u32,
}

pub fn run(options: &HashMap<String, String>) {
    let n: u32 = options.get("n").map_or(300, |v| v.parse().unwrap());
    let k: usize = options.get("bf").map_or(2, |v| v.parse().unwrap());
    let variant = options.get("variant").map_or("layout", String::as_str).to_string();
    let burst = options.get("mode").is_some_and(|m| m == "burst");

    let h = Harness::open(&page_url(&format!("s12.html?{variant}")), 1280, 720);
    let probes: Rc<RefCell<Vec<Point>>> = Rc::default();
    let log: Rc<RefCell<Vec<Delivery>>> = Rc::default();
    {
        let (probes, log, shared) = (probes.clone(), log.clone(), h.shared.clone());
        h.set_paint(move |info| {
            let frame = shared.gpu.import_frame(info);
            let origin = (info.extra.visible_rect.x as u32, info.extra.visible_rect.y as u32);
            if let Ok(dir) = std::env::var("HARNESS_DUMP") {
                let bytes = shared.gpu.read_frame(&frame, origin, (frame.width, frame.height));
                crate::gpu::save_png(&std::path::Path::new(&dir).join(format!("s12-{}.png", info.extra.capture_counter)), &bytes, frame.swap_rb, frame.width, frame.height);
            }
            let pixels = shared.gpu.read_points(&frame, origin, &probes.borrow());
            log.borrow_mut().push(Delivery { counter: info.extra.capture_counter, pixels });
        });
    }

    let prime = |i: u32, probes: &Rc<RefCell<Vec<Point>>>| {
        let mut points: Vec<Point> = expected(&variant, i).iter().map(|(p, _)| *p).collect();
        let previous = if i == 0 { 65535 } else { i - 1 };
        points.extend(expected(&variant, previous).iter().map(|(p, _)| *p));
        *probes.borrow_mut() = points;
    };

    prime(0, &probes);
    h.eval("seek(65535)");
    for _ in 0..3 {
        h.begin_frame();
        h.pump_for(Duration::from_millis(100));
    }
    log.borrow_mut().clear();

    let width = expected(&variant, 0).len();
    let mut tallies = vec![Tally::default(); k];
    let mut deliveries_per_frame = [0u32; 8];
    for i in 0..n {
        prime(i, &probes);
        h.eval(&format!("seek({i})"));
        let start = log.borrow().len();
        let classify = |tally: &mut Tally| {
            let log = log.borrow();
            match log.len() > start {
                false => tally.none += 1,
                true => {
                    let pixels = &log.last().unwrap().pixels;
                    let previous = if i == 0 { 65535 } else { i - 1 };
                    if matches(&variant, i, pixels, 0) {
                        tally.correct += 1;
                    } else if matches(&variant, previous, pixels, width) {
                        tally.stale += 1;
                    } else {
                        tally.other += 1;
                    }
                }
            }
        };
        if burst {
            for _ in 0..k {
                h.begin_frame();
            }
            h.pump_until(Duration::from_millis(250), || log.borrow().len() > start);
            let mut seen = log.borrow().len();
            while h.pump_until(Duration::from_millis(60), || log.borrow().len() > seen) {
                seen = log.borrow().len();
            }
            classify(&mut tallies[k - 1]);
        } else {
            for (j, tally) in tallies.iter_mut().enumerate() {
                let before = log.borrow().len();
                h.begin_frame();
                let wait = if j == 0 { 250 } else { 60 };
                h.pump_until(Duration::from_millis(wait), || log.borrow().len() > before);
                classify(tally);
            }
        }
        h.pump_for(Duration::from_millis(5));
        deliveries_per_frame[(log.borrow().len() - start).min(7)] += 1;
    }
    let log = log.borrow();
    if std::env::var("HARNESS_DEBUG").is_ok() {
        for (i, d) in log.iter().enumerate().take(6) {
            println!("  debug frame {i}: probes {:?} -> {:?}", prime_points(&variant, i as u32), d.pixels);
        }
    }
    let gaps = log.windows(2).filter(|w| w[1].counter != w[0].counter + 1).count();
    println!(
        "spike=1.2 variant={variant} mode={} bf={k} frames={n} raster={} load={}",
        if burst { "burst" } else { "sequential" },
        crate::harness::raster_mode(),
        crate::load_average()
    );
    for (j, t) in tallies.iter().enumerate() {
        let label = if burst { format!("after_burst_of_{k}") } else { format!("after_bf{}", j + 1) };
        if burst && j + 1 != k {
            continue;
        }
        println!("  {label}: shows_seek={} stale={} no_delivery={} other={}", t.correct, t.stale, t.none, t.other);
    }
    println!("  deliveries_per_frame histogram (0..7+): {deliveries_per_frame:?} capture_counter_gaps={gaps}");
    h.close();
}
