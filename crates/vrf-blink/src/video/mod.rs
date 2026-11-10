#[cfg_attr(windows, path = "decoder_win.rs")]
mod decoder;
mod hyperframes;
mod jpeg;
mod link;
pub mod mp4;
mod renderer;
mod stream;

#[cfg(windows)]
pub use decoder::set_hardware_decoding;
pub use decoder::{Clip, Frame, Matrix, Reader};
pub use hyperframes::NativeVideos;
pub use link::{Clips, Link, Pusher, Resolve, StartFallback};
pub(crate) use renderer::{INIT, PAGE, PUSH, VideoRenderHandler};
pub use stream::Stream;
