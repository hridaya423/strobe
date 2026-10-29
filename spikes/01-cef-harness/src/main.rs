mod s11;
mod s12;

use cef::{args::Args, *};
use harness::{Harness, HarnessApp};
use std::{collections::HashMap, process::ExitCode, time::Duration};

pub fn parse_options(args: &[String]) -> HashMap<String, String> {
    args.iter()
        .filter_map(|a| a.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

pub fn load_average() -> String {
    let out = std::process::Command::new("uptime").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    text.split("load averages:").nth(1).unwrap_or("?").trim().to_string()
}

pub fn page_url(name: &str) -> String {
    let root = std::env::var("HARNESS_PAGES").unwrap_or_else(|_| {
        format!("{}/pages", env!("CARGO_MANIFEST_DIR"))
    });
    format!("file://{root}/{name}")
}

fn main() -> ExitCode {
    let is_helper = std::env::args().any(|a| a.starts_with("--type="));
    let exe = std::env::current_exe().unwrap();
    let loader = library_loader::LibraryLoader::new(&exe, is_helper);
    assert!(loader.load(), "cannot load the CEF framework");
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);

    if !is_helper {
        mac::setup_application();
    }

    let args = Args::new();
    let mut app = HarnessApp::new();
    let ret = execute_process(Some(args.as_main_args()), Some(&mut app), std::ptr::null_mut());
    if is_helper {
        return ExitCode::from(ret.max(0) as u8);
    }
    assert_eq!(ret, -1, "browser process expected");

    let cache = std::env::temp_dir().join(format!("vrf-harness-{}", std::process::id()));
    let settings = Settings {
        no_sandbox: true as _,
        windowless_rendering_enabled: true as _,
        external_message_pump: true as _,
        root_cache_path: cache.to_str().unwrap().into(),
        log_severity: LogSeverity::from(sys::cef_log_severity_t::LOGSEVERITY_WARNING),
        log_file: cache.join("cef.log").to_str().unwrap().into(),
        ..Default::default()
    };
    assert_eq!(
        initialize(Some(args.as_main_args()), Some(&settings), Some(&mut app), std::ptr::null_mut()),
        1
    );

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let spike = argv.first().map(String::as_str).unwrap_or("smoke");
    let options = parse_options(&argv[1.min(argv.len())..]);
    match spike {
        "smoke" => {
            let h = Harness::open("about:blank", 640, 360);
            println!("eval 1+1 = {}", h.eval("1+1"));
            h.set_paint(|info| println!("paint counter={} size={}x{}", info.extra.capture_counter, info.extra.coded_size.width, info.extra.coded_size.height));
            h.begin_frame();
            h.pump_for(Duration::from_millis(500));
            println!("deliveries={}", h.deliveries());
            h.close();
        }
        "1.1" => s11::run(&options),
        "1.2" => s12::run(&options),
        other => panic!("unknown spike {other}"),
    }
    shutdown();
    let _ = std::fs::remove_dir_all(&cache);
    ExitCode::SUCCESS
}
