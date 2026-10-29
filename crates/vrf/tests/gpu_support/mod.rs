#[cfg(windows)]
mod d3d11;
#[cfg(target_os = "macos")]
mod metal;

#[cfg(windows)]
pub use d3d11::*;
#[cfg(target_os = "macos")]
pub use metal::*;
