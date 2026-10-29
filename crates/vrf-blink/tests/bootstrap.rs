#[allow(dead_code)]
mod support;

use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use vrf_blink::{Ui, View};

fn one_begin_frame_one_iosurface(ui: &Ui) -> Result<()> {
    let view = View::open(ui, "about:blank", 1280, 720)?;
    view.invalidate();
    view.begin_frame();
    let paint = view
        .recv_paint(Duration::from_secs(5))
        .context("no OnAcceleratedPaint within 5 s")?;
    ensure!(paint.surface.id() != 0, "IOSurface has no id");
    ensure!(
        (paint.surface.width(), paint.surface.height()) == (1280, 720),
        "IOSurface is {}x{}",
        paint.surface.width(),
        paint.surface.height()
    );
    ensure!(
        view.recv_paint(Duration::from_millis(300)).is_none(),
        "a second delivery followed one begin frame"
    );
    view.close()
}

fn main() -> ExitCode {
    support::run(&[(
        "bootstrap_one_begin_frame_one_iosurface",
        one_begin_frame_one_iosurface,
    )])
}
