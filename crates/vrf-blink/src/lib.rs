pub mod adapters;
mod app;
#[cfg(target_os = "macos")]
mod bundle;
pub mod calibration;
pub mod daemon;
pub mod driver;
#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
mod pipeline_mac;
#[cfg(windows)]
mod pipeline_win;
mod scheme;
pub mod video;
mod view;
#[cfg(windows)]
mod win;

pub use app::{OnUi, Ui, cpu_raster, run, set_cpu_raster};
pub use scheme::{Mode, Project};
pub use view::{Paint, View};

pub(crate) fn canonical(path: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let path = path.canonicalize()?;
    #[cfg(windows)]
    if let Some(plain) = path.to_str().and_then(|text| text.strip_prefix(r"\\?\")) {
        if !plain.starts_with("UNC\\") {
            return Ok(plain.into());
        }
    }
    Ok(path)
}
