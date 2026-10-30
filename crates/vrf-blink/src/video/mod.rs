#[cfg_attr(windows, path = "decoder_win.rs")]
mod decoder;
mod hyperframes;
mod link;
pub mod mp4;
mod renderer;
mod stream;

pub use decoder::{Clip, Frame, Matrix, Reader};
#[cfg(windows)]
pub use decoder::set_hardware_decoding;
pub use hyperframes::NativeVideos;
pub use link::{Clips, Link, Pusher, Resolve, StartFallback};
pub(crate) use renderer::{PAGE, PUSH, VideoRenderHandler};
pub use stream::Stream;
