use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::slice;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, anyhow, ensure};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D10::ID3D10Multithread;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFAttributes, IMFDXGIBuffer, IMFDXGIDeviceManager, IMFMediaBuffer, IMFMediaType,
    IMFSample, IMFSourceReader, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_MINIMUM_DISPLAY_APERTURE,
    MF_MT_SUBTYPE, MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_YUV_MATRIX, MF_SOURCE_READER_ALL_STREAMS,
    MF_SOURCE_READER_D3D_MANAGER, MF_SOURCE_READER_DISABLE_DXVA,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED,
    MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READERF_ERROR, MF_VERSION, MFCreateAttributes,
    MFCreateDXGIDeviceManager, MFCreateMediaType, MFCreateSourceReaderFromURL, MFMediaType_Video,
    MFSTARTUP_FULL, MFStartup, MFVideoFormat_NV12,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::core::{GUID, HSTRING, Interface};

use super::mp4::{self, Track};

const HUNDRED_NS: i128 = 10_000_000;
const MAIN_STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
const NORMAL_RANGE: u32 = 1;

static HARDWARE: AtomicBool = AtomicBool::new(true);

pub fn set_hardware_decoding(on: bool) {
    HARDWARE.store(on, Ordering::Relaxed);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Matrix {
    Bt709,
    Bt601,
}

fn matrix_of_colr(code: u16) -> Option<Matrix> {
    match code {
        0 | 2 => None,
        1 => Some(Matrix::Bt709),
        _ => Some(Matrix::Bt601),
    }
}

fn matrix_of_media_type(code: u32) -> Matrix {
    match code {
        0 | 1 => Matrix::Bt709,
        _ => Matrix::Bt601,
    }
}

pub struct Clip {
    path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub timescale: i32,
    pub pts: Vec<i64>,
    pub keyframes: Vec<usize>,
    pub matrix: Matrix,
    pub full_range: bool,
    offset: i64,
    codec: [u8; 4],
    profile: i32,
    edits: usize,
}

struct D3d {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    manager: IMFDXGIDeviceManager,
}

struct Media {
    d3d: Option<D3d>,
}

unsafe impl Send for Media {}
unsafe impl Sync for Media {}

fn d3d() -> Result<D3d> {
    unsafe {
        let mut device = None;
        let mut context = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            Default::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
        let device = device.context("no device")?;
        let context = context.context("no context")?;
        let _ = device
            .cast::<ID3D10Multithread>()?
            .SetMultithreadProtected(true);
        let mut token = 0;
        let mut manager = None;
        MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
        let manager = manager.context("no device manager")?;
        manager.ResetDevice(&device, token)?;
        Ok(D3d {
            device,
            context,
            manager,
        })
    }
}

fn media() -> Result<&'static Media> {
    static MEDIA: OnceLock<std::result::Result<Media, String>> = OnceLock::new();
    com();
    MEDIA
        .get_or_init(|| {
            unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }
                .map_err(|error| format!("starting Media Foundation: {error}"))?;
            Ok(Media { d3d: d3d().ok() })
        })
        .as_ref()
        .map_err(|error| anyhow!("{error}"))
}

fn com() {
    thread_local! {
        static INIT: () = unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        };
    }
    INIT.with(|_| ());
}

fn ticks(hns: i64, timescale: i64) -> i64 {
    ((hns as i128 * timescale as i128 + HUNDRED_NS / 2).div_euclid(HUNDRED_NS)) as i64
}

fn hundred_ns(ticks: i64, timescale: i64) -> i64 {
    ((ticks as i128 * HUNDRED_NS + timescale as i128 - 1).div_euclid(timescale as i128)) as i64
}

struct Output {
    width: u32,
    height: u32,
    left: usize,
    top: usize,
    matrix: u32,
    full_range: bool,
}

impl Output {
    fn read(ty: &IMFMediaType) -> Result<Output> {
        let packed = unsafe { ty.GetUINT64(&MF_MT_FRAME_SIZE)? };
        let (mut width, mut height) = ((packed >> 32) as u32, packed as u32);
        let (mut left, mut top) = (0, 0);
        let mut area = [0u8; 16];
        if unsafe { ty.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, &mut area, None) }.is_ok() {
            let value = |at: usize| i16::from_le_bytes([area[at], area[at + 1]]);
            let size = |at: usize| i32::from_le_bytes(area[at..at + 4].try_into().unwrap());
            left = value(2).max(0) as usize;
            top = value(6).max(0) as usize;
            if size(8) > 0 && size(12) > 0 {
                (width, height) = (size(8) as u32, size(12) as u32);
            }
        }
        Ok(Output {
            width,
            height,
            left,
            top,
            matrix: unsafe { ty.GetUINT32(&MF_MT_YUV_MATRIX) }.unwrap_or(0),
            full_range: unsafe { ty.GetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE) }.unwrap_or(0)
                == NORMAL_RANGE,
        })
    }
}

struct Staging {
    texture: ID3D11Texture2D,
    desc: D3D11_TEXTURE2D_DESC,
}

struct Source {
    reader: IMFSourceReader,
    output: Output,
    first: Option<(i64, IMFSample)>,
    staging: Option<Staging>,
}

unsafe impl Send for Source {}

impl Source {
    fn open(
        path: &Path,
        from: Option<i64>,
        timescale: i32,
        accept: impl Fn(i64) -> bool,
    ) -> Result<Source> {
        let hardware = HARDWARE.load(Ordering::Relaxed);
        let mut last = anyhow!("no attempt was made");
        for attempt in [hardware, hardware, false] {
            match Source::start(path, from, attempt) {
                Ok(source) => {
                    let tick = ticks(source.first_time(), timescale as i64);
                    if accept(tick) {
                        return Ok(source);
                    }
                    last = anyhow!("the decoder's first timestamp is {tick}");
                }
                Err(error) => last = error,
            }
        }
        Err(last)
    }

    fn first_time(&self) -> i64 {
        self.first.as_ref().map_or(0, |(time, _)| *time)
    }

    fn start(path: &Path, from: Option<i64>, hardware: bool) -> Result<Source> {
        let media = media()?;
        unsafe {
            let mut attributes: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attributes, 2)?;
            let attributes = attributes.context("no attributes")?;
            if hardware {
                let d3d = media.d3d.as_ref().context("no hardware device")?;
                attributes.SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, &d3d.manager)?;
            } else {
                attributes.SetUINT32(&MF_SOURCE_READER_DISABLE_DXVA, 1)?;
            }
            let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), &attributes)
                .context("cannot open the file")?;
            let wanted = MFCreateMediaType()?;
            wanted.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            wanted.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
            reader.SetStreamSelection(MAIN_STREAM, true)?;
            reader
                .SetCurrentMediaType(MAIN_STREAM, None, &wanted)
                .context("no decoder gives NV12")?;
            if let Some(from) = from {
                reader.SetCurrentPosition(&GUID::zeroed(), &PROPVARIANT::from(from))?;
            }
            let output = Output::read(&reader.GetCurrentMediaType(MAIN_STREAM)?)?;
            let mut source = Source {
                reader,
                output,
                first: None,
                staging: None,
            };
            source.first = Some(source.fetch()?.context("the file has no frames")?);
            if hardware {
                let (_, sample) = source.first.as_ref().expect("just fetched");
                ensure!(
                    sample.GetBufferByIndex(0)?.cast::<IMFDXGIBuffer>().is_ok(),
                    "the decoder gave memory, not a texture"
                );
            }
            Ok(source)
        }
    }

    fn fetch(&mut self) -> Result<Option<(i64, IMFSample)>> {
        loop {
            let (mut flags, mut time, mut sample) = (0u32, 0i64, None);
            unsafe {
                self.reader.ReadSample(
                    MAIN_STREAM,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut time),
                    Some(&mut sample),
                )?;
                ensure!(
                    flags & MF_SOURCE_READERF_ERROR.0 as u32 == 0,
                    "decoding failed"
                );
                if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32 != 0 {
                    self.output = Output::read(&self.reader.GetCurrentMediaType(MAIN_STREAM)?)?;
                }
            }
            if let Some(sample) = sample {
                return Ok(Some((time, sample)));
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                return Ok(None);
            }
        }
    }

    fn next(&mut self) -> Result<Option<(i64, IMFSample)>> {
        match self.first.take() {
            Some(first) => Ok(Some(first)),
            None => self.fetch(),
        }
    }

    fn pixels(&mut self, sample: &IMFSample, width: u32, height: u32) -> Result<Vec<u8>> {
        ensure!(
            self.output.width == width && self.output.height == height,
            "the decoder gives {}x{}, the clip is {width}x{height}",
            self.output.width,
            self.output.height
        );
        unsafe {
            let buffer = sample.GetBufferByIndex(0)?;
            if let Ok(shared) = buffer.cast::<IMFDXGIBuffer>() {
                return self.read_texture(&shared, width, height);
            }
            self.read_memory(sample, width, height)
        }
    }

    unsafe fn read_texture(
        &mut self,
        shared: &IMFDXGIBuffer,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>> {
        let d3d = media()?.d3d.as_ref().context("no hardware device")?;
        let mut raw = null_mut();
        unsafe { shared.GetResource(&ID3D11Texture2D::IID, &mut raw)? };
        let texture = unsafe { ID3D11Texture2D::from_raw(raw) };
        let index = unsafe { shared.GetSubresourceIndex()? };
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };
        if self.staging.as_ref().is_none_or(|staging| {
            (staging.desc.Width, staging.desc.Height, staging.desc.Format)
                != (desc.Width, desc.Height, desc.Format)
        }) {
            let wanted = D3D11_TEXTURE2D_DESC {
                MipLevels: 1,
                ArraySize: 1,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
                ..desc
            };
            let mut staging = None;
            unsafe {
                d3d.device
                    .CreateTexture2D(&wanted, None, Some(&mut staging))?
            };
            self.staging = Some(Staging {
                texture: staging.context("no staging texture")?,
                desc: wanted,
            });
        }
        let staging = self.staging.as_ref().expect("just made");
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            d3d.context
                .CopySubresourceRegion(&staging.texture, 0, 0, 0, 0, &texture, index, None);
            d3d.context
                .Map(&staging.texture, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        }
        let pitch = mapped.RowPitch as usize;
        let base = mapped.pData.cast::<u8>();
        let packed = unsafe {
            pack(
                base,
                base.add(pitch * desc.Height as usize),
                pitch,
                &self.output,
                width as usize,
                height as usize,
            )
        };
        unsafe { d3d.context.Unmap(&staging.texture, 0) };
        packed
    }

    unsafe fn read_memory(
        &mut self,
        sample: &IMFSample,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>> {
        let buffer: IMFMediaBuffer = unsafe { sample.ConvertToContiguousBuffer()? };
        let flat = buffer
            .cast::<IMF2DBuffer>()
            .context("the decoder's buffer has no pitch")?;
        let (mut base, mut pitch) = (null_mut(), 0i32);
        let length = unsafe {
            flat.Lock2D(&mut base, &mut pitch)?;
            flat.GetContiguousLength()?
        };
        let packed = (|| {
            ensure!(pitch > 0, "the decoder's picture is upside down");
            let pitch = pitch as usize;
            let rows = length as usize * 2 / 3 / pitch;
            ensure!(rows >= height as usize, "the decoder's buffer is too short");
            unsafe {
                pack(
                    base,
                    base.add(pitch * rows),
                    pitch,
                    &self.output,
                    width as usize,
                    height as usize,
                )
            }
        })();
        unsafe { flat.Unlock2D()? };
        packed
    }
}

unsafe fn pack(
    luma: *const u8,
    chroma: *const u8,
    pitch: usize,
    output: &Output,
    width: usize,
    height: usize,
) -> Result<Vec<u8>> {
    let (chroma_width, chroma_height) = (width.div_ceil(2), height.div_ceil(2));
    ensure!(
        pitch >= output.left + width,
        "the decoder's rows are shorter than the picture"
    );
    let mut out = Vec::with_capacity(Frame::nv12_len(width as u32, height as u32));
    for row in 0..height {
        out.extend_from_slice(unsafe {
            slice::from_raw_parts(luma.add((output.top + row) * pitch + output.left), width)
        });
    }
    for row in 0..chroma_height {
        out.extend_from_slice(unsafe {
            slice::from_raw_parts(
                chroma.add((output.top / 2 + row) * pitch + output.left / 2 * 2),
                chroma_width * 2,
            )
        });
    }
    Ok(out)
}

impl Clip {
    pub fn open(path: &Path) -> Result<Clip> {
        com();
        let Track {
            width,
            height,
            timescale,
            pts,
            media,
            keyframes,
            codec,
            profile,
            tags,
            lead,
            edits,
        } = mp4::read(path)?;
        let tagged = tags.and_then(|tags| matrix_of_colr(tags.matrix));
        let mut clip = Clip {
            path: path.to_owned(),
            width,
            height,
            timescale,
            pts,
            keyframes,
            matrix: tagged.unwrap_or(Matrix::Bt709),
            full_range: tags.and_then(|tags| tags.full_range).unwrap_or(false),
            offset: 0,
            codec,
            profile,
            edits,
        };
        if clip.refusal().is_some() {
            return Ok(clip);
        }
        let offsets = [0, -lead, media[0] - clip.pts[0]];
        let first = clip.pts[0];
        let source = Source::open(path, None, timescale, |tick| {
            offsets.iter().any(|offset| tick - offset == first)
        })
        .context("the decoder's first timestamp does not follow the container's")?;
        let tick = ticks(source.first_time(), timescale as i64);
        clip.offset = offsets
            .into_iter()
            .find(|offset| tick - offset == clip.pts[0])
            .with_context(|| {
                format!(
                    "the decoder's first timestamp {tick} does not match the container's {}",
                    clip.pts[0]
                )
            })?;
        ensure!(
            source.output.width == width && source.output.height == height,
            "the decoder gives {}x{}, the container says {width}x{height}",
            source.output.width,
            source.output.height
        );
        if tagged.is_none() {
            clip.matrix = matrix_of_media_type(source.output.matrix);
        }
        if tags.and_then(|tags| tags.full_range).is_none() {
            clip.full_range = source.output.full_range;
        }
        Ok(clip)
    }

    pub fn refusal(&self) -> Option<String> {
        if self.edits > 1 {
            return Some("the clip's edit list has several segments".into());
        }
        let supported = match &self.codec {
            b"avc1" => matches!(self.profile, 66 | 77 | 88 | 100),
            b"hvc1" | b"hev1" => matches!(self.profile & 0x1f, 1 | 3),
            _ => false,
        };
        (!supported).then(|| {
            format!(
                "{} profile {} is not 8-bit 4:2:0 H.264 or HEVC",
                String::from_utf8_lossy(&self.codec),
                self.profile
            )
        })
    }
}

pub struct Frame(Vec<u8>);

impl Frame {
    pub fn nv12_len(width: u32, height: u32) -> usize {
        let (cw, ch) = (width.div_ceil(2) as usize, height.div_ceil(2) as usize);
        width as usize * height as usize + 2 * cw * ch
    }

    pub fn write_nv12(&self, out: &mut [u8]) {
        out[..self.0.len()].copy_from_slice(&self.0);
    }
}

pub struct Reader {
    source: Source,
    first: usize,
    produced: usize,
}

impl Reader {
    pub fn open(clip: &Clip, from: usize) -> Result<Reader> {
        ensure!(from < clip.pts.len(), "frame {from} is past the end");
        com();
        let keyframe = clip
            .keyframes
            .partition_point(|key| *key <= from)
            .saturating_sub(1);
        let start = clip.pts[clip.keyframes.get(keyframe).copied().unwrap_or(0)];
        let position =
            (start != clip.pts[0]).then(|| hundred_ns(start + clip.offset, clip.timescale as i64));
        let source = Source::open(&clip.path, position, clip.timescale, |tick| {
            tick - clip.offset <= clip.pts[from]
        })?;
        Ok(Reader {
            source,
            first: from,
            produced: 0,
        })
    }

    pub fn next(&mut self, clip: &Clip) -> Result<Option<(usize, Frame)>> {
        loop {
            let Some((time, sample)) = self.source.next()? else {
                return Ok(None);
            };
            let pts = ticks(time, clip.timescale as i64) - clip.offset;
            if self.produced == 0 && pts < clip.pts[self.first] {
                continue;
            }
            let index = self.first + self.produced;
            ensure!(
                clip.pts.get(index) == Some(&pts),
                "the decoder gave pts {pts} for frame {index}, the table says {:?}",
                clip.pts.get(index)
            );
            self.produced += 1;
            let pixels = self.source.pixels(&sample, clip.width, clip.height)?;
            return Ok(Some((index, Frame(pixels))));
        }
    }
}
