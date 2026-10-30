#[allow(dead_code)]
mod support;

use std::path::Path;
use std::process::{Command, ExitCode};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use support::{fixture_project, options, work_dir};
use vrf::gpu::Gpu;
use vrf_bench::engines::{headless_shell, node_bin};
use vrf_bench::score;
use vrf_blink::adapters::remotion::{self, NEEDS_SOFTWARE_WEBGL, Page};
use vrf_blink::{Mode, Project, Ui};

fn open(ui: &Ui) -> Result<(vrf_blink::View, std::path::PathBuf)> {
    let project = fixture_project("webgl")?;
    let bundle = remotion::bundle(&project, Some(Path::new("src/index.jsx")))?;
    let page = Page::new(
        Project::open(&bundle.dir, Mode::Offline)?,
        &options(&project, "Shader"),
        None,
    )?;
    let (view, _) = page.open(ui, 320, 180)?;
    Ok((view, project))
}

fn software_webgl_equals_remotion_still(ui: &Ui) -> Result<()> {
    let (mut view, project) = open(ui)?;
    let out = work_dir().join("shader-still.png");
    let output = Command::new(node_bin(&project, "remotion"))
        .current_dir(&project)
        .env("REMOTION_DISABLE_TELEMETRY", "1")
        .args(["still", "src/index.jsx", "Shader"])
        .arg(&out)
        .args(["--frame=5", "--image-format", "png", "--browser-executable"])
        .arg(headless_shell())
        .output()?;
    ensure!(output.status.success(), "remotion still failed");
    let gpu = Gpu::new()?;
    for frame in 0..=5 {
        Page::seek(&mut view, frame, 1)?;
        view.begin_frame();
        view.recv_paint(Duration::from_secs(5))
            .context("no delivery")
            .and_then(|paint| {
                let texture = gpu.import_iosurface(&paint.surface, 0, paint.format)?;
                if frame < 5 {
                    return Ok(());
                }
                let ours = score::Rgb8 {
                    width: 320,
                    height: 180,
                    data: gpu
                        .read_texture(&texture)?
                        .chunks_exact(4)
                        .flat_map(|bgra| [bgra[2], bgra[1], bgra[0]])
                        .collect(),
                };
                let delta = score::compare(&ours, &score::Rgb8::load_png(&out)?)?.max_delta;
                ensure!(
                    delta == 0,
                    "WebGL frame differs from remotion still by up to {delta}"
                );
                Ok(())
            })?;
    }
    view.close()
}

fn main() -> ExitCode {
    vrf_blink::set_cpu_raster(true);
    let software = std::env::var_os("VRF_TEST_GPU_WEBGL").is_none();
    vrf_blink::set_software_webgl(software);
    if software {
        support::run(&[(
            "webgl_software_equals_still",
            software_webgl_equals_remotion_still,
        )])
    } else {
        support::run(&[("webgl_gpu_asks_for_software", gpu_webgl_is_reported)])
    }
}

fn gpu_webgl_is_reported(ui: &Ui) -> Result<()> {
    let project = fixture_project("webgl")?;
    let bundle = remotion::bundle(&project, Some(Path::new("src/index.jsx")))?;
    let page = Page::new(
        Project::open(&bundle.dir, Mode::Offline)?,
        &options(&project, "Shader"),
        None,
    )?;
    let error = page
        .open(ui, 320, 180)
        .err()
        .context("a GPU WebGL page was opened")?;
    ensure!(
        format!("{error:#}").contains(NEEDS_SOFTWARE_WEBGL),
        "unexpected error: {error:#}"
    );
    Ok(())
}
