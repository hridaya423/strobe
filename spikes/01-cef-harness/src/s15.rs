use crate::{gpu::save_png, harness::Harness, page_url};
use std::{cell::RefCell, collections::HashMap, path::PathBuf, rc::Rc, time::Duration};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;

pub fn run(options: &HashMap<String, String>) {
    let n: u32 = options.get("n").map_or(300, |v| v.parse().unwrap());
    let bf: usize = options.get("bf").map_or(1, |v| v.parse().unwrap());
    let out = PathBuf::from(options.get("out").expect("out=<dir>"));
    let label = options.get("label").expect("label=<mode-runN>").clone();
    let keep_all = options.get("keep").is_some_and(|v| v == "all");
    let reference: Option<Vec<String>> = options.get("against").map(|path| {
        std::fs::read_to_string(path).unwrap().lines().map(|l| l.split_whitespace().nth(1).unwrap().to_string()).collect()
    });
    std::fs::create_dir_all(out.join("frames")).unwrap();

    let h = Harness::open(&page_url("s15.html"), WIDTH as i32, HEIGHT as i32);
    let latest: Rc<RefCell<Option<Vec<u8>>>> = Rc::default();
    let path_used: Rc<RefCell<&'static str>> = Rc::new(RefCell::new("none"));
    {
        let (latest, shared, path_used) = (latest.clone(), h.shared.clone(), path_used.clone());
        h.set_paint(move |info| {
            let frame = shared.gpu.import_frame(info);
            let origin = (info.extra.visible_rect.x as u32, info.extra.visible_rect.y as u32);
            let mut bytes = shared.gpu.read_frame(&frame, origin, (WIDTH, HEIGHT));
            if frame.swap_rb {
                bytes.chunks_exact_mut(4).for_each(|p| p.swap(0, 2));
            }
            *path_used.borrow_mut() = "accelerated (IOSurface)";
            *latest.borrow_mut() = Some(bytes);
        });
    }
    {
        let (latest, path_used) = (latest.clone(), path_used.clone());
        h.set_bitmap(move |bytes, width, height| {
            assert_eq!((width as u32, height as u32), (WIDTH, HEIGHT));
            let mut rgba = bytes.to_vec();
            rgba.chunks_exact_mut(4).for_each(|p| p.swap(0, 2));
            *path_used.borrow_mut() = "software bitmap (OnPaint)";
            *latest.borrow_mut() = Some(rgba);
        });
    }

    h.eval("seek(100000)");
    for _ in 0..4 {
        let before = h.deliveries();
        h.begin_frame();
        h.pump_until(Duration::from_secs(5), || h.deliveries() > before);
        h.pump_for(Duration::from_millis(50));
    }

    let mut lines = Vec::new();
    let mut missing = 0;
    let mut differing_saved = 0;
    for i in 0..n {
        h.eval(&format!("seek({i})"));
        *latest.borrow_mut() = None;
        for j in 0..bf {
            let before = h.deliveries();
            h.begin_frame();
            h.pump_until(Duration::from_millis(if j == 0 { 400 } else { 60 }), || h.deliveries() > before);
        }
        let Some(bytes) = latest.borrow_mut().take() else {
            missing += 1;
            lines.push(format!("{i} none"));
            continue;
        };
        let hash = blake3::hash(&bytes).to_hex().to_string();
        let differs = reference.as_ref().is_some_and(|r| r.get(i as usize).is_none_or(|expected| *expected != hash));
        if keep_all || (differs && differing_saved < 30) {
            differing_saved += differs as u32;
            save_png(&out.join("frames").join(format!("{label}-{i:03}.png")), &bytes, false, WIDTH, HEIGHT);
        }
        lines.push(format!("{i} {hash}"));
    }
    std::fs::write(out.join(format!("{label}.hashes")), lines.join("\n") + "\n").unwrap();
    println!("spike=1.5 label={label} raster={} frames={n} bf={bf} missing={missing} delivery_path={} load={}", crate::harness::raster_mode(), path_used.borrow(), crate::load_average());
    h.close();
}
