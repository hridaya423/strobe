use std::time::Duration;

use anyhow::Result;
use vrf_blink::{Paint, View};

const PAGE: &str = concat!(
    "file://",
    env!("CARGO_MANIFEST_DIR"),
    "/../../crates/vrf-blink/tests/pages/colour.html"
);

fn centre(paint: &Paint) -> [i32; 3] {
    let surface = &paint.surface;
    unsafe {
        surface.lock(objc2_io_surface::IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut());
    }
    let (w, h) = (surface.width(), surface.height());
    let stride = surface.bytes_per_row();
    let base = surface.base_address().as_ptr() as *const u8;
    let px = unsafe { std::slice::from_raw_parts(base.add((h / 2) * stride + (w / 2) * 4), 4) };
    let out = [px[2] as i32, px[1] as i32, px[0] as i32];
    unsafe {
        surface.unlock(objc2_io_surface::IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut());
    }
    out
}

fn hash(paint: &Paint) -> u64 {
    let surface = &paint.surface;
    unsafe {
        surface.lock(objc2_io_surface::IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut());
    }
    let (w, h) = (surface.width(), surface.height());
    let stride = surface.bytes_per_row();
    let base = surface.base_address().as_ptr() as *const u8;
    let mut acc: u64 = 0xcbf29ce484222325;
    for y in 0..h {
        let row = unsafe { std::slice::from_raw_parts(base.add(y * stride), w * 4) };
        for chunk in row.chunks_exact(8) {
            acc = (acc ^ u64::from_le_bytes(chunk.try_into().unwrap())).wrapping_mul(0x100000001b3);
        }
    }
    unsafe {
        surface.unlock(objc2_io_surface::IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut());
    }
    acc
}

const TICK_OPACITY: &str = "(() => { const raf = window.requestAnimationFrame.bind(window); const e = document.createElement('div'); e.style.cssText = 'position:fixed;left:0;top:0;width:1px;height:1px;background:#000;opacity:0.0005;pointer-events:none;z-index:2147483647;will-change:opacity'; document.documentElement.appendChild(e); let n = 0; const tick = () => { e.style.opacity = (n++ & 1) ? '0.0005' : '0.001'; raf(tick); }; raf(tick); })()";
const TICK_BLEND: &str = "(() => { const raf = window.requestAnimationFrame.bind(window); const e = document.createElement('div'); e.style.cssText = 'position:fixed;left:0;top:0;width:1px;height:1px;background:#000;mix-blend-mode:difference;pointer-events:none;z-index:2147483647'; document.documentElement.appendChild(e); let n = 0; const tick = () => { e.style.width = (n++ & 1) ? '1px' : '2px'; raf(tick); }; raf(tick); })()";

fn expected(index: u64) -> [i32; 3] {
    let digit = |d: u64| 16 + 10 * d as i32;
    [digit(index % 24), digit(index / 24 % 24), digit(index / 576)]
}

fn near(a: [i32; 3], b: [i32; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() <= 3)
}

#[derive(Default)]
struct Tally {
    frames: u64,
    changed: u64,
    unchanged: u64,
    one_fresh: u64,
    stale_first: u64,
    extra_deliveries: u64,
    none: u64,
    gaps: u64,
    lat_changed: Vec<f64>,
    lat_unchanged: Vec<f64>,
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn pct(v: &mut Vec<f64>, p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v.get(((v.len().max(1) - 1) as f64 * p) as usize).copied().unwrap_or(0.0)
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "inval-after".into());
    let period: u64 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(4);
    let frames: u64 = std::env::args().nth(3).and_then(|s| s.parse().ok()).unwrap_or(400);
    let quiet_ms: u64 = std::env::args().nth(4).and_then(|s| s.parse().ok()).unwrap_or(5);
    vrf_blink::run(move |ui| -> Result<()> {
        let page = std::env::var("NODAMAGE_PAGE").unwrap_or_else(|_| PAGE.to_owned());
        let mut view = View::open(&ui, &page, 1920, 1080)?;
        if mode == "tick-opacity" { view.evaluate(TICK_OPACITY)?; }
        if mode == "tick-blend" { view.evaluate(TICK_BLEND)?; }
        let mut hashes: Vec<u64> = Vec::new();
        let mut tally = Tally::default();
        let mut last_counter: Option<u64> = None;
        for i in 0..frames {
            let content = i - i % period;
            let prev_content = if content >= period { content - period } else { 0 };
            let changed = i % period == 0;
            view.evaluate(&format!("seek({content})"))?;
            let sent;
            let mut deliveries: Vec<(Duration, [i32; 3], u64)> = Vec::new();
            match mode.as_str() {
                "baseline" | "tick-opacity" | "tick-blend" => {
                    sent = view.begin_frame();
                    if let Some(p) = view.recv_paint(Duration::from_millis(500)) {
                        deliveries.push((p.at - sent, centre(&p), p.counter));
                        hashes.push(hash(&p));
                    }
                    if mode != "baseline" {
                        while let Some(p) = view.recv_paint(Duration::from_millis(20)) {
                            deliveries.push((p.at - sent, centre(&p), p.counter));
                        }
                    }
                }
                "inval-after" => {
                    sent = view.begin_frame();
                    view.invalidate();
                    while let Some(p) = view.recv_paint(Duration::from_millis(40)) {
                        deliveries.push((p.at - sent, centre(&p), p.counter));
                    }
                }
                "inval-before" => {
                    view.invalidate();
                    sent = view.begin_frame();
                    while let Some(p) = view.recv_paint(Duration::from_millis(40)) {
                        deliveries.push((p.at - sent, centre(&p), p.counter));
                    }
                }
                "wait-inval" => {
                    sent = view.begin_frame();
                    let first = view.recv_paint(Duration::from_millis(quiet_ms));
                    match first {
                        Some(p) => deliveries.push((p.at - sent, centre(&p), p.counter)),
                        None => view.invalidate(),
                    }
                    while let Some(p) = view.recv_paint(Duration::from_millis(40)) {
                        deliveries.push((p.at - sent, centre(&p), p.counter));
                    }
                }
                other => panic!("unknown mode {other}"),
            }
            tally.frames += 1;
            if changed { tally.changed += 1 } else { tally.unchanged += 1 }
            if let Some(h) = hashes.last().copied().filter(|_| deliveries.is_empty()) { hashes.push(h); }
            if deliveries.is_empty() {
                tally.none += 1;
                if changed { eprintln!("frame {i}: no delivery for a changed frame") }
                continue;
            }
            let (lat, colour, _) = deliveries[0];
            let fresh = near(colour, expected(content));
            let stale = near(colour, expected(prev_content)) && changed;
            if changed { tally.lat_changed.push(ms(lat)) } else { tally.lat_unchanged.push(ms(lat)) }
            if stale { tally.stale_first += 1 }
            if deliveries.len() == 1 && fresh { tally.one_fresh += 1 }
            tally.extra_deliveries += deliveries.len() as u64 - 1;
            for d in &deliveries {
                if let Some(last) = last_counter && d.2 != last + 1 { tally.gaps += 1 }
                last_counter = Some(d.2);
            }
            if i < 12 || (deliveries.len() > 1 && tally.extra_deliveries < 6) {
                eprintln!(
                    "frame {i} changed={changed} deliveries={:?}",
                    deliveries.iter().map(|d| (format!("{:.2}", ms(d.0)), d.1, d.2)).collect::<Vec<_>>()
                );
            }
        }
        if let Some(path) = std::env::args().nth(5) {
            std::fs::write(path, hashes.iter().map(|h| format!("{h:016x}\n")).collect::<String>())?;
        }
        view.close()?;
        println!(
            "mode={mode} period={period} frames={} changed={} unchanged={} one_fresh={} stale_first={} extra={} none={} counter_gaps={} lat_changed p50/p99/max={:.2}/{:.2}/{:.2} lat_unchanged p50/p99/max={:.2}/{:.2}/{:.2}",
            tally.frames, tally.changed, tally.unchanged, tally.one_fresh, tally.stale_first,
            tally.extra_deliveries, tally.none, tally.gaps,
            pct(&mut tally.lat_changed, 0.5), pct(&mut tally.lat_changed, 0.99), pct(&mut tally.lat_changed, 1.0),
            pct(&mut tally.lat_unchanged, 0.5), pct(&mut tally.lat_unchanged, 0.99), pct(&mut tally.lat_unchanged, 1.0),
        );
        Ok(())
    })
    .unwrap();
}
