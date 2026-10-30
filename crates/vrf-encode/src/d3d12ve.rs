use std::collections::VecDeque;
use std::mem::{ManuallyDrop, size_of};

use anyhow::{Context, Result, ensure};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_12_0;
use windows::Win32::Graphics::Direct3D12::{
    D3D12_COMMAND_LIST_FLAG_NONE, D3D12_COMMAND_LIST_TYPE_COPY,
    D3D12_COMMAND_LIST_TYPE_VIDEO_ENCODE, D3D12_COMMAND_QUEUE_DESC, D3D12_FENCE_FLAG_NONE,
    D3D12_HEAP_FLAG_NONE, D3D12_HEAP_PROPERTIES, D3D12_HEAP_TYPE_DEFAULT, D3D12_HEAP_TYPE_READBACK,
    D3D12_RANGE, D3D12_RESOURCE_BARRIER, D3D12_RESOURCE_BARRIER_0,
    D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES, D3D12_RESOURCE_BARRIER_FLAG_NONE,
    D3D12_RESOURCE_BARRIER_TYPE_TRANSITION, D3D12_RESOURCE_DESC, D3D12_RESOURCE_DIMENSION_BUFFER,
    D3D12_RESOURCE_DIMENSION_TEXTURE2D, D3D12_RESOURCE_FLAG_DENY_SHADER_RESOURCE,
    D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_FLAG_VIDEO_ENCODE_REFERENCE_ONLY,
    D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_DEST,
    D3D12_RESOURCE_STATE_VIDEO_ENCODE_READ, D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE,
    D3D12_RESOURCE_STATES, D3D12_RESOURCE_TRANSITION_BARRIER, D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
    D3D12_TEXTURE_LAYOUT_UNKNOWN, D3D12CreateDevice, ID3D12CommandAllocator, ID3D12CommandList,
    ID3D12CommandQueue, ID3D12Device, ID3D12Device4, ID3D12Fence, ID3D12GraphicsCommandList,
    ID3D12Resource,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_NV12, DXGI_FORMAT_UNKNOWN, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1,
};
use windows::Win32::Media::MediaFoundation::{
    D3D12_FEATURE_DATA_VIDEO_ENCODER_CODEC,
    D3D12_FEATURE_DATA_VIDEO_ENCODER_RESOLUTION_SUPPORT_LIMITS,
    D3D12_FEATURE_DATA_VIDEO_ENCODER_RESOURCE_REQUIREMENTS,
    D3D12_FEATURE_DATA_VIDEO_ENCODER_SUPPORT, D3D12_FEATURE_VIDEO_ENCODER_CODEC,
    D3D12_FEATURE_VIDEO_ENCODER_RESOURCE_REQUIREMENTS, D3D12_FEATURE_VIDEO_ENCODER_SUPPORT,
    D3D12_VIDEO_ENCODE_REFERENCE_FRAMES, D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION,
    D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_0, D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264,
    D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_DIRECT_MODES_DISABLED,
    D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_FLAG_ENABLE_CABAC_ENCODING,
    D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_SLICES_DEBLOCKING_MODE_0_ALL_LUMA_CHROMA_SLICE_BLOCK_EDGES_ALWAYS_FILTERED,
    D3D12_VIDEO_ENCODER_CODEC_H264, D3D12_VIDEO_ENCODER_COMPRESSED_BITSTREAM,
    D3D12_VIDEO_ENCODER_DESC, D3D12_VIDEO_ENCODER_ENCODE_OPERATION_METADATA_BUFFER,
    D3D12_VIDEO_ENCODER_ENCODEFRAME_INPUT_ARGUMENTS,
    D3D12_VIDEO_ENCODER_ENCODEFRAME_OUTPUT_ARGUMENTS, D3D12_VIDEO_ENCODER_FLAG_NONE,
    D3D12_VIDEO_ENCODER_FRAME_SUBREGION_LAYOUT_MODE_FULL_FRAME,
    D3D12_VIDEO_ENCODER_FRAME_TYPE_H264_IDR_FRAME, D3D12_VIDEO_ENCODER_FRAME_TYPE_H264_P_FRAME,
    D3D12_VIDEO_ENCODER_HEAP_DESC, D3D12_VIDEO_ENCODER_HEAP_FLAG_NONE,
    D3D12_VIDEO_ENCODER_INTRA_REFRESH, D3D12_VIDEO_ENCODER_INTRA_REFRESH_MODE_NONE,
    D3D12_VIDEO_ENCODER_LEVEL_SETTING, D3D12_VIDEO_ENCODER_LEVEL_SETTING_0,
    D3D12_VIDEO_ENCODER_LEVELS_H264, D3D12_VIDEO_ENCODER_LEVELS_H264_41,
    D3D12_VIDEO_ENCODER_LEVELS_H264_51,
    D3D12_VIDEO_ENCODER_MOTION_ESTIMATION_PRECISION_MODE_MAXIMUM,
    D3D12_VIDEO_ENCODER_OUTPUT_METADATA, D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA,
    D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_0,
    D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_H264,
    D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_H264_FLAG_NONE,
    D3D12_VIDEO_ENCODER_PICTURE_CONTROL_DESC,
    D3D12_VIDEO_ENCODER_PICTURE_CONTROL_FLAG_USED_AS_REFERENCE_PICTURE,
    D3D12_VIDEO_ENCODER_PICTURE_CONTROL_SUBREGIONS_LAYOUT_DATA,
    D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC, D3D12_VIDEO_ENCODER_PROFILE_DESC,
    D3D12_VIDEO_ENCODER_PROFILE_DESC_0, D3D12_VIDEO_ENCODER_PROFILE_H264,
    D3D12_VIDEO_ENCODER_PROFILE_H264_MAIN, D3D12_VIDEO_ENCODER_RATE_CONTROL,
    D3D12_VIDEO_ENCODER_RATE_CONTROL_CONFIGURATION_PARAMS,
    D3D12_VIDEO_ENCODER_RATE_CONTROL_CONFIGURATION_PARAMS_0, D3D12_VIDEO_ENCODER_RATE_CONTROL_CQP,
    D3D12_VIDEO_ENCODER_RATE_CONTROL_FLAG_NONE, D3D12_VIDEO_ENCODER_RATE_CONTROL_MODE_CQP,
    D3D12_VIDEO_ENCODER_RECONSTRUCTED_PICTURE,
    D3D12_VIDEO_ENCODER_REFERENCE_PICTURE_DESCRIPTOR_H264,
    D3D12_VIDEO_ENCODER_RESOLVE_METADATA_INPUT_ARGUMENTS,
    D3D12_VIDEO_ENCODER_RESOLVE_METADATA_OUTPUT_ARGUMENTS,
    D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_DESC, D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_FLAG_NONE,
    D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE, D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_0,
    D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_H264,
    D3D12_VIDEO_ENCODER_SUPPORT_FLAG_GENERAL_SUPPORT_OK, ID3D12VideoDevice3,
    ID3D12VideoEncodeCommandList2, ID3D12VideoEncoder, ID3D12VideoEncoderHeap,
};
use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
use windows::core::Interface;

use crate::bitstream;
use crate::nvenc::{InputPool, PixelFormat};
use crate::{Codec, Encoder, EncoderConfig, EncoderFrame, Packet, Quality, Rational, StreamInfo};

const LOG2_MAX_FRAME_NUM_MINUS4: u8 = 4;
const SPS_ID: u32 = 0;
const PPS_ID: u32 = 0;

struct Bits {
    bytes: Vec<u8>,
    used: u32,
}

impl Bits {
    fn new() -> Bits {
        Bits {
            bytes: Vec::new(),
            used: 0,
        }
    }

    fn bit(&mut self, value: bool) {
        if self.used == 0 {
            self.bytes.push(0);
        }
        if value {
            *self.bytes.last_mut().unwrap() |= 0x80 >> self.used;
        }
        self.used = (self.used + 1) % 8;
    }

    fn bits(&mut self, value: u32, count: u32) {
        for shift in (0..count).rev() {
            self.bit(value >> shift & 1 == 1);
        }
    }

    fn ue(&mut self, value: u32) {
        let code = value + 1;
        let length = 32 - code.leading_zeros();
        self.bits(0, length - 1);
        self.bits(code, length);
    }

    fn se(&mut self, value: i32) {
        self.ue(if value > 0 {
            2 * value as u32 - 1
        } else {
            (-2 * value) as u32
        });
    }

    fn finish(mut self) -> Vec<u8> {
        self.bit(true);
        while self.used != 0 {
            self.bit(false);
        }
        self.bytes
    }
}

fn nal(header: u8, rbsp: Vec<u8>) -> Vec<u8> {
    let mut out = vec![0, 0, 0, 1, header];
    let mut zeros = 0;
    for byte in rbsp {
        if zeros >= 2 && byte <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    out
}

fn level_idc(width: u32, height: u32) -> (u32, D3D12_VIDEO_ENCODER_LEVELS_H264) {
    if u64::from(width) * u64::from(height) <= 1920 * 1088 {
        (41, D3D12_VIDEO_ENCODER_LEVELS_H264_41)
    } else {
        (51, D3D12_VIDEO_ENCODER_LEVELS_H264_51)
    }
}

fn sps(width: u32, height: u32, level: u32) -> Vec<u8> {
    let (mb_width, mb_height) = (width.div_ceil(16), height.div_ceil(16));
    let (crop_right, crop_bottom) = ((mb_width * 16 - width) / 2, (mb_height * 16 - height) / 2);
    let mut b = Bits::new();
    b.bits(77, 8);
    b.bits(0, 8);
    b.bits(level, 8);
    b.ue(SPS_ID);
    b.ue(u32::from(LOG2_MAX_FRAME_NUM_MINUS4));
    b.ue(2);
    b.ue(1);
    b.bit(false);
    b.ue(mb_width - 1);
    b.ue(mb_height - 1);
    b.bit(true);
    b.bit(true);
    let crop = crop_right > 0 || crop_bottom > 0;
    b.bit(crop);
    if crop {
        b.ue(0);
        b.ue(crop_right);
        b.ue(0);
        b.ue(crop_bottom);
    }
    b.bit(false);
    nal(0x67, b.finish())
}

fn pps() -> Vec<u8> {
    let mut b = Bits::new();
    b.ue(PPS_ID);
    b.ue(SPS_ID);
    b.bit(true);
    b.bit(false);
    b.ue(0);
    b.ue(0);
    b.ue(0);
    b.bit(false);
    b.bits(0, 2);
    b.se(0);
    b.se(0);
    b.se(0);
    b.bit(true);
    b.bit(false);
    b.bit(false);
    nal(0x68, b.finish())
}

fn quantisers(quality: Quality) -> (u32, u32) {
    match quality {
        Quality::Draft => (31, 33),
        Quality::Balanced => (26, 28),
        Quality::High => (22, 24),
    }
}

fn borrowed(resource: &ID3D12Resource) -> ManuallyDrop<Option<ID3D12Resource>> {
    unsafe { std::mem::transmute_copy(resource) }
}

fn transition(
    resource: &ID3D12Resource,
    before: D3D12_RESOURCE_STATES,
    after: D3D12_RESOURCE_STATES,
) -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                pResource: borrowed(resource),
                Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                StateBefore: before,
                StateAfter: after,
            }),
        },
    }
}

fn buffer(
    device: &ID3D12Device,
    size: u64,
    heap: windows::Win32::Graphics::Direct3D12::D3D12_HEAP_TYPE,
    state: D3D12_RESOURCE_STATES,
) -> Result<ID3D12Resource> {
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Width: size,
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_UNKNOWN,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        Flags: D3D12_RESOURCE_FLAG_NONE,
        ..Default::default()
    };
    let mut resource: Option<ID3D12Resource> = None;
    unsafe {
        device.CreateCommittedResource(
            &D3D12_HEAP_PROPERTIES {
                Type: heap,
                ..Default::default()
            },
            D3D12_HEAP_FLAG_NONE,
            &desc,
            state,
            None,
            &mut resource,
        )?;
    }
    resource.context("no buffer returned")
}

fn reference_texture(device: &ID3D12Device, width: u32, height: u32) -> Result<ID3D12Resource> {
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        Width: u64::from(width),
        Height: height,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
        Flags: D3D12_RESOURCE_FLAG_VIDEO_ENCODE_REFERENCE_ONLY
            | D3D12_RESOURCE_FLAG_DENY_SHADER_RESOURCE,
        ..Default::default()
    };
    let mut resource: Option<ID3D12Resource> = None;
    unsafe {
        device.CreateCommittedResource(
            &D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_DEFAULT,
                ..Default::default()
            },
            D3D12_HEAP_FLAG_NONE,
            &desc,
            D3D12_RESOURCE_STATE_COMMON,
            None,
            &mut resource,
        )?;
    }
    resource.context("no reference texture returned")
}

/// Every hardware adapter whose D3D12 device encodes H.264, with its name.
pub fn video_devices() -> Result<Vec<(String, ID3D12Device)>> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let mut found = Vec::new();
    for index in 0.. {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
            break;
        };
        let desc = unsafe { adapter.GetDesc1()? };
        if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
            continue;
        }
        let mut device: Option<ID3D12Device> = None;
        if unsafe { D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_12_0, &mut device) }.is_err() {
            continue;
        }
        let Some(device) = device else { continue };
        let Ok(video) = device.cast::<ID3D12VideoDevice3>() else {
            continue;
        };
        let mut codec = D3D12_FEATURE_DATA_VIDEO_ENCODER_CODEC {
            NodeIndex: 0,
            Codec: D3D12_VIDEO_ENCODER_CODEC_H264,
            IsSupported: false.into(),
        };
        let checked = unsafe {
            video.CheckFeatureSupport(
                D3D12_FEATURE_VIDEO_ENCODER_CODEC,
                (&mut codec as *mut D3D12_FEATURE_DATA_VIDEO_ENCODER_CODEC).cast(),
                size_of::<D3D12_FEATURE_DATA_VIDEO_ENCODER_CODEC>() as u32,
            )
        };
        if checked.is_ok() && codec.IsSupported.as_bool() {
            let name = String::from_utf16_lossy(&desc.Description);
            found.push((name.trim_end_matches('\0').to_string(), device));
        }
    }
    Ok(found)
}

struct Queue {
    queue: ID3D12CommandQueue,
    allocator: ID3D12CommandAllocator,
}

impl Queue {
    fn new(
        device: &ID3D12Device,
        kind: windows::Win32::Graphics::Direct3D12::D3D12_COMMAND_LIST_TYPE,
    ) -> Result<Queue> {
        Ok(Queue {
            queue: unsafe {
                device.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                    Type: kind,
                    ..Default::default()
                })?
            },
            allocator: unsafe { device.CreateCommandAllocator(kind)? },
        })
    }
}

/// An H.264 Main profile encoder on D3D12 Video Encode: constant QP, one reference frame, no
/// B-frames. Each `submit` encodes and reads back one frame before it returns.
pub struct D3d12H264Encoder {
    config: EncoderConfig,
    pool: InputPool,
    video_queue: Queue,
    video_list: ID3D12VideoEncodeCommandList2,
    copy_queue: Queue,
    copy_list: ID3D12GraphicsCommandList,
    fence: ID3D12Fence,
    fence_value: u64,
    event: HANDLE,
    encoder: ID3D12VideoEncoder,
    heap: ID3D12VideoEncoderHeap,
    references: [ID3D12Resource; 2],
    bitstream: ID3D12Resource,
    bitstream_readback: ID3D12Resource,
    bitstream_size: u64,
    layout: ID3D12Resource,
    resolved: ID3D12Resource,
    resolved_readback: ID3D12Resource,
    level: u32,
    quantisers: (u32, u32),
    in_gop: u32,
    idr_id: u32,
    last_pts: Option<Rational>,
    packets: VecDeque<Packet>,
    record: Option<Vec<u8>>,
}

impl D3d12H264Encoder {
    pub fn new(
        device: &ID3D12Device,
        config: &EncoderConfig,
        pool_size: usize,
    ) -> Result<D3d12H264Encoder> {
        ensure!(
            config.codec == Codec::H264 && !config.ten_bit,
            "D3D12 Video Encode here does 8-bit H.264 only"
        );
        let (width, height) = (config.width, config.height);
        ensure!(
            width % 2 == 0 && height % 2 == 0,
            "{width}x{height} is not a 4:2:0 size"
        );
        let video: ID3D12VideoDevice3 = device
            .cast()
            .context("the device has no D3D12 video encode support")?;
        let (level_idc, level_enum) = level_idc(width, height);

        let mut profile = D3D12_VIDEO_ENCODER_PROFILE_H264_MAIN;
        let profile_desc = D3D12_VIDEO_ENCODER_PROFILE_DESC {
            DataSize: size_of::<D3D12_VIDEO_ENCODER_PROFILE_H264>() as u32,
            Anonymous: D3D12_VIDEO_ENCODER_PROFILE_DESC_0 {
                pH264Profile: &mut profile,
            },
        };
        let mut h264 = D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264 {
            ConfigurationFlags: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_FLAG_ENABLE_CABAC_ENCODING,
            DirectModeConfig: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_DIRECT_MODES_DISABLED,
            DisableDeblockingFilterConfig: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_SLICES_DEBLOCKING_MODE_0_ALL_LUMA_CHROMA_SLICE_BLOCK_EDGES_ALWAYS_FILTERED,
        };
        let codec_configuration = D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION {
            DataSize: size_of::<D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264>() as u32,
            Anonymous: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_0 {
                pH264Config: &mut h264,
            },
        };
        let mut level_value = level_enum;
        let level_setting = D3D12_VIDEO_ENCODER_LEVEL_SETTING {
            DataSize: size_of::<D3D12_VIDEO_ENCODER_LEVELS_H264>() as u32,
            Anonymous: D3D12_VIDEO_ENCODER_LEVEL_SETTING_0 {
                pH264LevelSetting: &mut level_value,
            },
        };
        let mut gop = gop_structure(config);
        let gop_desc = D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE {
            DataSize: size_of::<D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_H264>() as u32,
            Anonymous: D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_0 {
                pH264GroupOfPictures: &mut gop,
            },
        };
        let quantisers = quantisers(config.quality);
        let cqp = D3D12_VIDEO_ENCODER_RATE_CONTROL_CQP {
            ConstantQP_FullIntracodedFrame: quantisers.0,
            ConstantQP_InterPredictedFrame_PrevRefOnly: quantisers.1,
            ConstantQP_InterPredictedFrame_BiDirectionalRef: quantisers.1,
        };
        let rate_control = rate_control(config, &cqp);
        let resolution = D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC {
            Width: width,
            Height: height,
        };

        let mut support = D3D12_FEATURE_DATA_VIDEO_ENCODER_SUPPORT {
            NodeIndex: 0,
            Codec: D3D12_VIDEO_ENCODER_CODEC_H264,
            InputFormat: DXGI_FORMAT_NV12,
            CodecConfiguration: codec_configuration,
            CodecGopSequence: gop_desc,
            RateControl: rate_control,
            IntraRefresh: D3D12_VIDEO_ENCODER_INTRA_REFRESH_MODE_NONE,
            SubregionFrameEncoding: D3D12_VIDEO_ENCODER_FRAME_SUBREGION_LAYOUT_MODE_FULL_FRAME,
            ResolutionsListCount: 1,
            pResolutionList: &resolution,
            MaxReferenceFramesInDPB: 1,
            ..Default::default()
        };
        let mut limits = [D3D12_FEATURE_DATA_VIDEO_ENCODER_RESOLUTION_SUPPORT_LIMITS::default()];
        support.pResolutionDependentSupport = limits.as_mut_ptr();
        support.SuggestedProfile = profile_desc;
        support.SuggestedLevel = level_setting;
        unsafe {
            video
                .CheckFeatureSupport(
                    D3D12_FEATURE_VIDEO_ENCODER_SUPPORT,
                    (&mut support as *mut D3D12_FEATURE_DATA_VIDEO_ENCODER_SUPPORT).cast(),
                    size_of::<D3D12_FEATURE_DATA_VIDEO_ENCODER_SUPPORT>() as u32,
                )
                .context("querying encoder support")?;
        }
        ensure!(
            support.SupportFlags.0 & D3D12_VIDEO_ENCODER_SUPPORT_FLAG_GENERAL_SUPPORT_OK.0 != 0,
            "the encoder rejects this configuration: validation flags {:#x}",
            support.ValidationFlags.0
        );

        let encoder: ID3D12VideoEncoder = unsafe {
            video
                .CreateVideoEncoder(&D3D12_VIDEO_ENCODER_DESC {
                    NodeMask: 0,
                    Flags: D3D12_VIDEO_ENCODER_FLAG_NONE,
                    EncodeCodec: D3D12_VIDEO_ENCODER_CODEC_H264,
                    EncodeProfile: profile_desc,
                    InputFormat: DXGI_FORMAT_NV12,
                    CodecConfiguration: codec_configuration,
                    MaxMotionEstimationPrecision:
                        D3D12_VIDEO_ENCODER_MOTION_ESTIMATION_PRECISION_MODE_MAXIMUM,
                })
                .context("creating the video encoder")?
        };
        let heap: ID3D12VideoEncoderHeap = unsafe {
            video
                .CreateVideoEncoderHeap(&D3D12_VIDEO_ENCODER_HEAP_DESC {
                    NodeMask: 0,
                    Flags: D3D12_VIDEO_ENCODER_HEAP_FLAG_NONE,
                    EncodeCodec: D3D12_VIDEO_ENCODER_CODEC_H264,
                    EncodeProfile: profile_desc,
                    EncodeLevel: level_setting,
                    ResolutionsListCount: 1,
                    pResolutionList: &resolution,
                })
                .context("creating the video encoder heap")?
        };

        let mut requirements = D3D12_FEATURE_DATA_VIDEO_ENCODER_RESOURCE_REQUIREMENTS {
            NodeIndex: 0,
            Codec: D3D12_VIDEO_ENCODER_CODEC_H264,
            Profile: profile_desc,
            InputFormat: DXGI_FORMAT_NV12,
            PictureTargetResolution: resolution,
            ..Default::default()
        };
        unsafe {
            video
                .CheckFeatureSupport(
                    D3D12_FEATURE_VIDEO_ENCODER_RESOURCE_REQUIREMENTS,
                    (&mut requirements
                        as *mut D3D12_FEATURE_DATA_VIDEO_ENCODER_RESOURCE_REQUIREMENTS)
                        .cast(),
                    size_of::<D3D12_FEATURE_DATA_VIDEO_ENCODER_RESOURCE_REQUIREMENTS>() as u32,
                )
                .context("querying encoder resource requirements")?;
        }
        ensure!(
            requirements.IsSupported.as_bool(),
            "the encoder reports no resource requirements for {width}x{height}"
        );
        let layout_size = u64::from(requirements.MaxEncoderOutputMetadataBufferSize).max(4096);
        let bitstream_size = (u64::from(width) * u64::from(height) * 3 / 2).next_multiple_of(4096);

        let video_queue = Queue::new(device, D3D12_COMMAND_LIST_TYPE_VIDEO_ENCODE)
            .context("creating the video encode queue")?;
        let copy_queue =
            Queue::new(device, D3D12_COMMAND_LIST_TYPE_COPY).context("creating the copy queue")?;
        let device4: ID3D12Device4 = device.cast()?;
        let video_list: ID3D12VideoEncodeCommandList2 = unsafe {
            device4.CreateCommandList1(
                0,
                D3D12_COMMAND_LIST_TYPE_VIDEO_ENCODE,
                D3D12_COMMAND_LIST_FLAG_NONE,
            )
        }
        .context("creating the video encode list")?;
        let copy_list: ID3D12GraphicsCommandList = unsafe {
            device4.CreateCommandList1(
                0,
                D3D12_COMMAND_LIST_TYPE_COPY,
                D3D12_COMMAND_LIST_FLAG_NONE,
            )
        }
        .context("creating the copy list")?;
        let fence: ID3D12Fence = unsafe { device.CreateFence(0, D3D12_FENCE_FLAG_NONE)? };
        let event = unsafe { CreateEventW(None, false, false, None)? };

        Ok(D3d12H264Encoder {
            pool: InputPool::unregistered(device, width, height, PixelFormat::Nv12, pool_size)
                .context("creating input textures")?,
            references: [
                reference_texture(device, width, height).context("creating a reference texture")?,
                reference_texture(device, width, height).context("creating a reference texture")?,
            ],
            bitstream: buffer(
                device,
                bitstream_size,
                D3D12_HEAP_TYPE_DEFAULT,
                D3D12_RESOURCE_STATE_COMMON,
            )
            .context("creating the bitstream buffer")?,
            bitstream_readback: buffer(
                device,
                bitstream_size,
                D3D12_HEAP_TYPE_READBACK,
                D3D12_RESOURCE_STATE_COPY_DEST,
            )
            .context("creating the bitstream readback")?,
            layout: buffer(
                device,
                layout_size,
                D3D12_HEAP_TYPE_DEFAULT,
                D3D12_RESOURCE_STATE_COMMON,
            )
            .context("creating the layout buffer")?,
            resolved: buffer(
                device,
                layout_size,
                D3D12_HEAP_TYPE_DEFAULT,
                D3D12_RESOURCE_STATE_COMMON,
            )
            .context("creating the resolved buffer")?,
            resolved_readback: buffer(
                device,
                layout_size,
                D3D12_HEAP_TYPE_READBACK,
                D3D12_RESOURCE_STATE_COPY_DEST,
            )
            .context("creating the resolved readback")?,
            bitstream_size,
            config: config.clone(),
            video_queue,
            video_list,
            copy_queue,
            copy_list,
            fence,
            fence_value: 0,
            event,
            encoder,
            heap,
            level: level_idc,
            quantisers,
            in_gop: 0,
            idr_id: 0,
            last_pts: None,
            packets: VecDeque::new(),
            record: None,
        })
    }

    pub fn pool(&self) -> &InputPool {
        &self.pool
    }

    fn wait(&self, value: u64) -> Result<()> {
        unsafe {
            if self.fence.GetCompletedValue() < value {
                self.fence.SetEventOnCompletion(value, self.event)?;
                WaitForSingleObject(self.event, INFINITE);
            }
        }
        Ok(())
    }

    fn encode(&mut self, input: &ID3D12Resource, idr: bool) -> Result<Vec<u8>> {
        let (width, height) = (self.config.width, self.config.height);
        let current = (self.in_gop % 2) as usize;
        let previous = 1 - current;
        let mut gop = gop_structure(&self.config);
        let cqp = D3D12_VIDEO_ENCODER_RATE_CONTROL_CQP {
            ConstantQP_FullIntracodedFrame: self.quantisers.0,
            ConstantQP_InterPredictedFrame_PrevRefOnly: self.quantisers.1,
            ConstantQP_InterPredictedFrame_BiDirectionalRef: self.quantisers.1,
        };
        let frame_num = self.in_gop;
        let mut list0 = [0u32];
        let mut descriptors = [D3D12_VIDEO_ENCODER_REFERENCE_PICTURE_DESCRIPTOR_H264 {
            ReconstructedPictureResourceIndex: 0,
            IsLongTermReference: false.into(),
            LongTermPictureIdx: 0,
            PictureOrderCountNumber: frame_num.saturating_sub(1) * 2,
            FrameDecodingOrderNumber: frame_num.saturating_sub(1),
            TemporalLayerIndex: 0,
        }];
        let mut picture = D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_H264 {
            Flags: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_H264_FLAG_NONE,
            FrameType: if idr {
                D3D12_VIDEO_ENCODER_FRAME_TYPE_H264_IDR_FRAME
            } else {
                D3D12_VIDEO_ENCODER_FRAME_TYPE_H264_P_FRAME
            },
            pic_parameter_set_id: PPS_ID,
            idr_pic_id: self.idr_id,
            PictureOrderCountNumber: frame_num * 2,
            FrameDecodingOrderNumber: frame_num,
            TemporalLayerIndex: 0,
            List0ReferenceFramesCount: u32::from(!idr),
            pList0ReferenceFrames: if idr {
                std::ptr::null_mut()
            } else {
                list0.as_mut_ptr()
            },
            ReferenceFramesReconPictureDescriptorsCount: u32::from(!idr),
            pReferenceFramesReconPictureDescriptors: if idr {
                std::ptr::null_mut()
            } else {
                descriptors.as_mut_ptr()
            },
            ..unsafe { std::mem::zeroed() }
        };
        let mut reference_textures: [Option<ID3D12Resource>; 1] =
            [Some(self.references[previous].clone())];
        let mut reference_subresources = [0u32];
        let flags = D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_FLAG_NONE;
        let input_arguments = D3D12_VIDEO_ENCODER_ENCODEFRAME_INPUT_ARGUMENTS {
            SequenceControlDesc: D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_DESC {
                Flags: flags,
                IntraRefreshConfig: D3D12_VIDEO_ENCODER_INTRA_REFRESH {
                    Mode: D3D12_VIDEO_ENCODER_INTRA_REFRESH_MODE_NONE,
                    IntraRefreshDuration: 0,
                },
                RateControl: rate_control(&self.config, &cqp),
                PictureTargetResolution: D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC {
                    Width: width,
                    Height: height,
                },
                SelectedLayoutMode: D3D12_VIDEO_ENCODER_FRAME_SUBREGION_LAYOUT_MODE_FULL_FRAME,
                FrameSubregionsLayoutData:
                    D3D12_VIDEO_ENCODER_PICTURE_CONTROL_SUBREGIONS_LAYOUT_DATA::default(),
                CodecGopSequence: D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE {
                    DataSize: size_of::<D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_H264>() as u32,
                    Anonymous: D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_0 {
                        pH264GroupOfPictures: &mut gop,
                    },
                },
            },
            PictureControlDesc: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_DESC {
                IntraRefreshFrameIndex: 0,
                Flags: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_FLAG_USED_AS_REFERENCE_PICTURE,
                PictureControlCodecData: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA {
                    DataSize: size_of::<D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_H264>()
                        as u32,
                    Anonymous: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_0 {
                        pH264PicData: &mut picture,
                    },
                },
                ReferenceFrames: D3D12_VIDEO_ENCODE_REFERENCE_FRAMES {
                    NumTexture2Ds: u32::from(!idr),
                    ppTexture2Ds: if idr {
                        std::ptr::null_mut()
                    } else {
                        reference_textures.as_mut_ptr()
                    },
                    pSubresources: if idr {
                        std::ptr::null_mut()
                    } else {
                        reference_subresources.as_mut_ptr()
                    },
                },
            },
            pInputFrame: borrowed(input),
            InputFrameSubresource: 0,
            CurrentFrameBitstreamMetadataSize: 0,
        };
        let output_arguments = D3D12_VIDEO_ENCODER_ENCODEFRAME_OUTPUT_ARGUMENTS {
            Bitstream: D3D12_VIDEO_ENCODER_COMPRESSED_BITSTREAM {
                pBuffer: borrowed(&self.bitstream),
                FrameStartOffset: 0,
            },
            ReconstructedPicture: D3D12_VIDEO_ENCODER_RECONSTRUCTED_PICTURE {
                pReconstructedPicture: borrowed(&self.references[current]),
                ReconstructedPictureSubresource: 0,
            },
            EncoderOutputMetadata: D3D12_VIDEO_ENCODER_ENCODE_OPERATION_METADATA_BUFFER {
                pBuffer: borrowed(&self.layout),
                Offset: 0,
            },
        };
        let mut profile = D3D12_VIDEO_ENCODER_PROFILE_H264_MAIN;
        let resolve_input = D3D12_VIDEO_ENCODER_RESOLVE_METADATA_INPUT_ARGUMENTS {
            EncoderCodec: D3D12_VIDEO_ENCODER_CODEC_H264,
            EncoderProfile: D3D12_VIDEO_ENCODER_PROFILE_DESC {
                DataSize: size_of::<D3D12_VIDEO_ENCODER_PROFILE_H264>() as u32,
                Anonymous: D3D12_VIDEO_ENCODER_PROFILE_DESC_0 {
                    pH264Profile: &mut profile,
                },
            },
            EncoderInputFormat: DXGI_FORMAT_NV12,
            EncodedPictureEffectiveResolution: D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC {
                Width: width,
                Height: height,
            },
            HWLayoutMetadata: D3D12_VIDEO_ENCODER_ENCODE_OPERATION_METADATA_BUFFER {
                pBuffer: borrowed(&self.layout),
                Offset: 0,
            },
        };
        let resolve_output = D3D12_VIDEO_ENCODER_RESOLVE_METADATA_OUTPUT_ARGUMENTS {
            ResolvedLayoutMetadata: D3D12_VIDEO_ENCODER_ENCODE_OPERATION_METADATA_BUFFER {
                pBuffer: borrowed(&self.resolved),
                Offset: 0,
            },
        };

        let common = D3D12_RESOURCE_STATE_COMMON;
        let read = D3D12_RESOURCE_STATE_VIDEO_ENCODE_READ;
        let write = D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE;
        let mut before = vec![
            transition(input, common, read),
            transition(&self.references[current], common, write),
            transition(&self.bitstream, common, write),
            transition(&self.layout, common, write),
        ];
        let mut after = vec![
            transition(input, read, common),
            transition(&self.references[current], write, common),
            transition(&self.bitstream, write, common),
            transition(&self.resolved, write, common),
        ];
        if !idr {
            before.push(transition(&self.references[previous], common, read));
            after.push(transition(&self.references[previous], read, common));
        }
        let middle = [
            transition(&self.layout, write, read),
            transition(&self.resolved, common, write),
        ];
        let layout_back = [transition(&self.layout, read, common)];

        unsafe {
            self.video_queue
                .allocator
                .Reset()
                .context("resetting the encode allocator")?;
            self.video_list
                .Reset(&self.video_queue.allocator)
                .context("resetting the encode list")?;
            self.video_list.ResourceBarrier(&before);
            self.video_list.EncodeFrame(
                &self.encoder,
                &self.heap,
                &input_arguments,
                &output_arguments,
            );
            self.video_list.ResourceBarrier(&middle);
            self.video_list
                .ResolveEncoderOutputMetadata(&resolve_input, &resolve_output);
            self.video_list.ResourceBarrier(&after);
            self.video_list.ResourceBarrier(&layout_back);
            self.video_list.Close().context(
                "closing the encode list (a recording error in EncodeFrame or the barriers)",
            )?;
            self.video_queue
                .queue
                .ExecuteCommandLists(&[Some(self.video_list.cast::<ID3D12CommandList>()?)]);
            self.fence_value += 1;
            self.video_queue
                .queue
                .Signal(&self.fence, self.fence_value)?;

            self.copy_queue
                .queue
                .Wait(&self.fence, self.fence_value)
                .context("copy queue wait")?;
            self.copy_queue
                .allocator
                .Reset()
                .context("resetting the copy allocator")?;
            self.copy_list
                .Reset(&self.copy_queue.allocator, None)
                .context("resetting the copy list")?;
            self.copy_list.CopyBufferRegion(
                &self.resolved_readback,
                0,
                &self.resolved,
                0,
                size_of::<D3D12_VIDEO_ENCODER_OUTPUT_METADATA>() as u64,
            );
            self.copy_list.CopyBufferRegion(
                &self.bitstream_readback,
                0,
                &self.bitstream,
                0,
                self.bitstream_size,
            );
            self.copy_list.Close().context("closing the copy list")?;
            self.copy_queue
                .queue
                .ExecuteCommandLists(&[Some(self.copy_list.cast::<ID3D12CommandList>()?)]);
            self.fence_value += 1;
            self.copy_queue
                .queue
                .Signal(&self.fence, self.fence_value)?;
        }
        self.wait(self.fence_value)?;

        for barrier in before
            .iter()
            .chain(&middle)
            .chain(&after)
            .chain(&layout_back)
        {
            drop(unsafe { ManuallyDrop::into_inner(barrier.Anonymous.Transition.clone()) });
        }

        let metadata = unsafe {
            let mut at = std::ptr::null_mut();
            self.resolved_readback.Map(
                0,
                Some(&D3D12_RANGE {
                    Begin: 0,
                    End: size_of::<D3D12_VIDEO_ENCODER_OUTPUT_METADATA>(),
                }),
                Some(&mut at),
            )?;
            let metadata = at
                .cast::<D3D12_VIDEO_ENCODER_OUTPUT_METADATA>()
                .read_unaligned();
            self.resolved_readback.Unmap(0, None);
            metadata
        };
        ensure!(
            metadata.EncodeErrorFlags == 0,
            "the encoder reported error flags {:#x}",
            metadata.EncodeErrorFlags
        );
        let written = metadata.EncodedBitstreamWrittenBytesCount;
        ensure!(
            written > 0 && written <= self.bitstream_size,
            "the encoder wrote {written} bytes"
        );
        let mut bytes = Vec::with_capacity(written as usize);
        unsafe {
            let mut at = std::ptr::null_mut();
            self.bitstream_readback.Map(
                0,
                Some(&D3D12_RANGE {
                    Begin: 0,
                    End: written as usize,
                }),
                Some(&mut at),
            )?;
            bytes.extend_from_slice(std::slice::from_raw_parts(
                at.cast::<u8>(),
                written as usize,
            ));
            self.bitstream_readback.Unmap(0, None);
        }
        Ok(bytes)
    }
}

fn gop_structure(config: &EncoderConfig) -> D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_H264 {
    D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_H264 {
        GOPLength: config.keyframe_interval,
        PPicturePeriod: 1,
        pic_order_cnt_type: 2,
        log2_max_frame_num_minus4: LOG2_MAX_FRAME_NUM_MINUS4,
        log2_max_pic_order_cnt_lsb_minus4: 0,
    }
}

fn rate_control(
    config: &EncoderConfig,
    cqp: &D3D12_VIDEO_ENCODER_RATE_CONTROL_CQP,
) -> D3D12_VIDEO_ENCODER_RATE_CONTROL {
    D3D12_VIDEO_ENCODER_RATE_CONTROL {
        Mode: D3D12_VIDEO_ENCODER_RATE_CONTROL_MODE_CQP,
        Flags: D3D12_VIDEO_ENCODER_RATE_CONTROL_FLAG_NONE,
        ConfigParams: D3D12_VIDEO_ENCODER_RATE_CONTROL_CONFIGURATION_PARAMS {
            DataSize: size_of::<D3D12_VIDEO_ENCODER_RATE_CONTROL_CQP>() as u32,
            Anonymous: D3D12_VIDEO_ENCODER_RATE_CONTROL_CONFIGURATION_PARAMS_0 {
                pConfiguration_CQP: cqp,
            },
        },
        TargetFrameRate: DXGI_RATIONAL {
            Numerator: config.frame_rate.num as u32,
            Denominator: config.frame_rate.den as u32,
        },
    }
}

impl Encoder for D3d12H264Encoder {
    fn submit(&mut self, frame: EncoderFrame, pts: Rational) -> Result<()> {
        let EncoderFrame::D3d12(frame) = frame;
        ensure!(
            frame.pool_is(&self.pool),
            "the frame comes from another encoder's pool"
        );
        ensure!(
            self.last_pts
                .is_none_or(|last| i128::from(pts.num) * i128::from(last.den)
                    > i128::from(last.num) * i128::from(pts.den)),
            "pts must increase with every frame"
        );
        if let Some(ready) = &frame.ready {
            unsafe { self.video_queue.queue.Wait(&ready.fence, ready.value)? };
        }
        let idr = self.in_gop == 0;
        let slices = self.encode(frame.resource(), idr)?;
        let mut stream = Vec::new();
        if idr {
            stream.extend(sps(self.config.width, self.config.height, self.level));
            stream.extend(pps());
        }
        stream.extend(slices);
        let access = bitstream::convert(Codec::H264, &stream)?;
        if self.record.is_none() && !access.parameters.is_empty() {
            self.record = Some(bitstream::config_record(
                Codec::H264,
                false,
                &access.parameters,
            )?);
        }
        self.packets.push_back(Packet {
            data: access.data,
            pts,
            dts: pts,
            keyframe: access.keyframe,
        });
        self.last_pts = Some(pts);
        self.in_gop += 1;
        if idr {
            self.idr_id = (self.idr_id + 1) % 65536;
        }
        if self.in_gop >= self.config.keyframe_interval.max(1) {
            self.in_gop = 0;
        }
        Ok(())
    }

    fn drain(&mut self) -> Result<Vec<Packet>> {
        Ok(self.packets.drain(..).collect())
    }

    fn flush(&mut self) -> Result<Vec<Packet>> {
        self.drain()
    }

    fn stream_info(&self) -> Result<StreamInfo> {
        Ok(StreamInfo {
            codec: Codec::H264,
            width: self.config.width,
            height: self.config.height,
            timebase: Rational::new(1, self.config.frame_rate.num),
            config: self.record.clone().context("no frame was encoded")?,
        })
    }

    fn finish(mut self) -> Result<StreamInfo> {
        let undrained = self.flush()?.len();
        ensure!(
            undrained == 0,
            "finish found {undrained} undrained packets; drain after flush"
        );
        self.stream_info()
    }
}

impl Drop for D3d12H264Encoder {
    fn drop(&mut self) {
        let _ = self.wait(self.fence_value);
        let _ = unsafe { CloseHandle(self.event) };
    }
}
