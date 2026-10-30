use std::path::Path;

use anyhow::{Result, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Matrix {
    Bt709,
    Bt601,
}

pub struct Clip {
    pub width: u32,
    pub height: u32,
    pub timescale: i32,
    pub pts: Vec<i64>,
    pub keyframes: Vec<usize>,
    pub matrix: Matrix,
    pub full_range: bool,
}

impl Clip {
    pub fn open(_: &Path) -> Result<Clip> {
        bail!("native video decoding is not available on Windows")
    }

    pub fn refusal(&self) -> Option<String> {
        Some("native video decoding is not available on Windows".into())
    }
}

pub struct Frame(());

impl Frame {
    pub fn nv12_len(width: u32, height: u32) -> usize {
        let (cw, ch) = (width.div_ceil(2) as usize, height.div_ceil(2) as usize);
        width as usize * height as usize + 2 * cw * ch
    }

    pub fn write_nv12(&self, _: &mut [u8]) {}
}

pub struct Reader(());

impl Reader {
    pub fn open(_: &Clip, _: usize) -> Result<Reader> {
        bail!("native video decoding is not available on Windows")
    }

    pub fn next(&mut self, _: &Clip) -> Result<Option<(usize, Frame)>> {
        Ok(None)
    }
}
