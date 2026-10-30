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
    #[cfg(target_os = "macos")]
    {
        ensure!(paint.surface.id() != 0, "IOSurface has no id");
        ensure!(
            (paint.surface.width(), paint.surface.height()) == (1280, 720),
            "IOSurface is {}x{}",
            paint.surface.width(),
            paint.surface.height()
        );
    }
    #[cfg(windows)]
    {
        let gpu = vrf::gpu::Gpu::new()?;
        let texture = gpu.import_shared_handle(&paint.surface, wgpu::TextureUsages::COPY_SRC)?;
        ensure!(
            (texture.width(), texture.height()) == (1280, 720),
            "the shared texture is {}x{}",
            texture.width(),
            texture.height()
        );
    }
    ensure!(
        view.recv_paint(Duration::from_millis(300)).is_none(),
        "a second delivery followed one begin frame"
    );
    view.close()
}

fn generic_families_resolve_as_in_headless_chromium(ui: &Ui) -> Result<()> {
    let mut view = View::open(ui, "about:blank", 320, 180)?;
    let measure = |view: &mut View, family: &str| {
        view.evaluate(&format!(
            "(() => {{ const c = document.createElement('canvas').getContext('2d'); c.font = '44px {family}'; const m = c.measureText('Frame gj'); return [m.width, m.fontBoundingBoxAscent, m.fontBoundingBoxDescent].join(); }})()"
        ))
    };
    for (generic, named) in [
        ("sans-serif", "Arial"),
        ("serif", "Times New Roman"),
        ("fantasy", "Impact"),
        ("cursive", "Script"),
    ] {
        let (got, want) = (measure(&mut view, generic)?, measure(&mut view, named)?);
        ensure!(
            got == want,
            "{generic} measures {got}, {named} measures {want}"
        );
    }
    view.close()
}

fn main() -> ExitCode {
    support::run(&[
        (
            "bootstrap_one_begin_frame_one_iosurface",
            one_begin_frame_one_iosurface,
        ),
        (
            "bootstrap_generic_families_resolve_as_in_headless_chromium",
            generic_families_resolve_as_in_headless_chromium,
        ),
    ])
}
