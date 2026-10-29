#[allow(dead_code)]
mod guid;
#[allow(dead_code)]
mod version;

use std::ffi::c_void;
use std::io::Write;
use std::time::Instant;

use libloading::Library;
use nvidia_video_codec_sdk::sys::nvEncodeAPI as nv;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_NV12, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
use windows::core::Interface;

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const UNIQUE_FRAMES: usize = 16;
const OUTPUTS_IN_FLIGHT: usize = 48;

struct Args {
    codec: String,
    preset: u32,
    tuning: String,
    frames: usize,
    list: bool,
    gpu_copy: bool,
    out: Option<String>,
}

impl Args {
    fn parse() -> Self {
        let mut a = Args {
            codec: "h264".into(),
            preset: 4,
            tuning: "ll".into(),
            frames: 600,
            list: false,
            gpu_copy: false,
            out: None,
        };
        let mut it = std::env::args().skip(1);
        while let Some(k) = it.next() {
            match k.as_str() {
                "--codec" => a.codec = it.next().unwrap(),
                "--preset" => a.preset = it.next().unwrap().parse().unwrap(),
                "--tuning" => a.tuning = it.next().unwrap(),
                "--frames" => a.frames = it.next().unwrap().parse().unwrap(),
                "--out" => a.out = it.next(),
                "--list" => a.list = true,
                "--gpu-copy" => a.gpu_copy = true,
                _ => {}
            }
        }
        a
    }
}

struct Api {
    f: nv::NV_ENCODE_API_FUNCTION_LIST,
    enc: *mut c_void,
}

impl Api {
    fn check(&self, status: nv::NVENCSTATUS, what: &str) {
        if status != nv::NVENCSTATUS::NV_ENC_SUCCESS {
            let msg = unsafe {
                let p = (self.f.nvEncGetLastErrorString.unwrap())(self.enc);
                if p.is_null() {
                    String::new()
                } else {
                    std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
                }
            };
            panic!("{what} failed: {status:?} {msg}");
        }
    }
}

unsafe fn zeroed<T>() -> T {
    unsafe { std::mem::MaybeUninit::<T>::zeroed().assume_init() }
}

fn codec_name(g: &nv::GUID) -> &'static str {
    if *g == guid::NV_ENC_CODEC_H264_GUID {
        "H.264"
    } else if *g == guid::NV_ENC_CODEC_HEVC_GUID {
        "HEVC"
    } else if *g == guid::NV_ENC_CODEC_AV1_GUID {
        "AV1"
    } else {
        "unknown"
    }
}

fn preset_guid(n: u32) -> nv::GUID {
    match n {
        1 => guid::NV_ENC_PRESET_P1_GUID,
        2 => guid::NV_ENC_PRESET_P2_GUID,
        3 => guid::NV_ENC_PRESET_P3_GUID,
        4 => guid::NV_ENC_PRESET_P4_GUID,
        5 => guid::NV_ENC_PRESET_P5_GUID,
        6 => guid::NV_ENC_PRESET_P6_GUID,
        _ => guid::NV_ENC_PRESET_P7_GUID,
    }
}

fn preset_name(g: &nv::GUID) -> String {
    (1..=7)
        .find(|n| preset_guid(*n) == *g)
        .map(|n| format!("P{n}"))
        .unwrap_or_else(|| "legacy".into())
}

fn caps(api: &Api, codec: nv::GUID, cap: nv::NV_ENC_CAPS) -> i32 {
    let mut p: nv::NV_ENC_CAPS_PARAM = unsafe { zeroed() };
    p.version = version::NV_ENC_CAPS_PARAM_VER;
    p.capsToQuery = cap;
    let mut v = 0i32;
    unsafe {
        (api.f.nvEncGetEncodeCaps.unwrap())(api.enc, codec, &mut p, &mut v);
    }
    v
}

fn list(api: &Api) {
    let mut n = 0u32;
    unsafe {
        api.check(
            (api.f.nvEncGetEncodeGUIDCount.unwrap())(api.enc, &mut n),
            "GUIDCount",
        );
        let mut guids = vec![zeroed::<nv::GUID>(); n as usize];
        let mut got = 0u32;
        api.check(
            (api.f.nvEncGetEncodeGUIDs.unwrap())(api.enc, guids.as_mut_ptr(), n, &mut got),
            "GetEncodeGUIDs",
        );
        println!("codecs reported: {}", got);
        for g in &guids[..got as usize] {
            let mut pc = 0u32;
            api.check(
                (api.f.nvEncGetEncodePresetCount.unwrap())(api.enc, *g, &mut pc),
                "PresetCount",
            );
            let mut presets = vec![zeroed::<nv::GUID>(); pc as usize];
            let mut pgot = 0u32;
            api.check(
                (api.f.nvEncGetEncodePresetGUIDs.unwrap())(
                    api.enc,
                    *g,
                    presets.as_mut_ptr(),
                    pc,
                    &mut pgot,
                ),
                "PresetGUIDs",
            );
            let names: Vec<String> = presets[..pgot as usize].iter().map(preset_name).collect();
            let p_levels: Vec<&String> = names.iter().filter(|n| n.starts_with('P')).collect();
            println!(
                "  {}: presets {:?} (+{} legacy)  max {}x{}  b-frames {}  engines {}  10-bit {}  yuv444 {}  lookahead {}  async {}",
                codec_name(g),
                p_levels,
                names.len() - p_levels.len(),
                caps(api, *g, nv::NV_ENC_CAPS::NV_ENC_CAPS_WIDTH_MAX),
                caps(api, *g, nv::NV_ENC_CAPS::NV_ENC_CAPS_HEIGHT_MAX),
                caps(api, *g, nv::NV_ENC_CAPS::NV_ENC_CAPS_NUM_MAX_BFRAMES),
                caps(api, *g, nv::NV_ENC_CAPS::NV_ENC_CAPS_NUM_ENCODER_ENGINES),
                caps(api, *g, nv::NV_ENC_CAPS::NV_ENC_CAPS_SUPPORT_10BIT_ENCODE),
                caps(api, *g, nv::NV_ENC_CAPS::NV_ENC_CAPS_SUPPORT_YUV444_ENCODE),
                caps(api, *g, nv::NV_ENC_CAPS::NV_ENC_CAPS_SUPPORT_LOOKAHEAD),
                caps(api, *g, nv::NV_ENC_CAPS::NV_ENC_CAPS_ASYNC_ENCODE_SUPPORT),
            );
        }
    }
}

fn committed(
    dev: &ID3D12Device,
    heap: D3D12_HEAP_TYPE,
    desc: D3D12_RESOURCE_DESC,
    state: D3D12_RESOURCE_STATES,
) -> ID3D12Resource {
    let mut r: Option<ID3D12Resource> = None;
    unsafe {
        dev.CreateCommittedResource(
            &D3D12_HEAP_PROPERTIES {
                Type: heap,
                ..Default::default()
            },
            D3D12_HEAP_FLAG_NONE,
            &desc,
            state,
            None,
            &mut r,
        )
        .unwrap();
    }
    r.unwrap()
}

fn buffer_desc(size: u64) -> D3D12_RESOURCE_DESC {
    D3D12_RESOURCE_DESC {
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
    }
}

fn wait_fence(fence: &ID3D12Fence, event: HANDLE, value: u64) {
    unsafe {
        if fence.GetCompletedValue() < value {
            fence.SetEventOnCompletion(value, event).unwrap();
            WaitForSingleObject(event, INFINITE);
        }
    }
}

fn hash(x: u32, y: u32, k: u32) -> u32 {
    let mut h =
        x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA6B) ^ k.wrapping_mul(0xC2B2_AE35);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^ (h >> 12)
}

fn fill_nv12(k: u32, y_plane: &mut [u8], y_pitch: usize, uv_plane: &mut [u8], uv_pitch: usize) {
    for y in 0..HEIGHT as usize {
        for x in 0..WIDTH as usize {
            let base = (x as u32 * 3 / 8 + y as u32 / 3 + k * 9) & 0xFF;
            let noise = hash(x as u32, y as u32, k) & 15;
            let block = (x as i32 - (k as i32 * 60) % 1700).unsigned_abs() < 100
                && (y as i32 - 400).unsigned_abs() < 100;
            y_plane[y * y_pitch + x] = if block {
                235
            } else {
                (16 + (base * 200 / 255) + noise).min(235) as u8
            };
        }
    }
    for y in 0..HEIGHT as usize / 2 {
        for x in 0..WIDTH as usize / 2 {
            uv_plane[y * uv_pitch + x * 2] = (64 + ((x as u32 + k * 5) & 127)) as u8;
            uv_plane[y * uv_pitch + x * 2 + 1] = (64 + ((y as u32 * 2 + k * 3) & 127)) as u8;
        }
    }
}

fn main() {
    let args = Args::parse();

    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1().unwrap() };
    let mut dev: Option<ID3D12Device> = None;
    let mut i = 0;
    while let Ok(a) = unsafe { factory.EnumAdapters1(i) } {
        i += 1;
        let d = unsafe { a.GetDesc1().unwrap() };
        let name = String::from_utf16_lossy(&d.Description)
            .trim_end_matches('\0')
            .to_string();
        if name.contains("NVIDIA") {
            println!("D3D12 device on: {name}");
            unsafe { D3D12CreateDevice(&a, D3D_FEATURE_LEVEL_11_0, &mut dev).unwrap() };
            break;
        }
    }
    let dev = dev.expect("no NVIDIA adapter");

    let lib = match unsafe { Library::new("nvEncodeAPI64.dll") } {
        Ok(l) => {
            println!("nvEncodeAPI64.dll: loaded");
            l
        }
        Err(e) => {
            println!("nvEncodeAPI64.dll: FAILED to load: {e}");
            return;
        }
    };
    let mut api = Api {
        f: unsafe { zeroed() },
        enc: std::ptr::null_mut(),
    };
    unsafe {
        let get_max: libloading::Symbol<unsafe extern "C" fn(*mut u32) -> nv::NVENCSTATUS> =
            lib.get(b"NvEncodeAPIGetMaxSupportedVersion").unwrap();
        let mut max = 0u32;
        get_max(&mut max);
        println!(
            "driver max NVENC API version: {}.{} (header built against {}.{})",
            max >> 4,
            max & 0xF,
            nv::NVENCAPI_MAJOR_VERSION,
            nv::NVENCAPI_MINOR_VERSION
        );
        let create: libloading::Symbol<
            unsafe extern "C" fn(*mut nv::NV_ENCODE_API_FUNCTION_LIST) -> nv::NVENCSTATUS,
        > = lib.get(b"NvEncodeAPICreateInstance").unwrap();
        api.f.version = version::NV_ENCODE_API_FUNCTION_LIST_VER;
        let st = create(&mut api.f);
        api.check(st, "NvEncodeAPICreateInstance");

        let mut open: nv::NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS = zeroed();
        open.version = version::NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER;
        open.deviceType = nv::NV_ENC_DEVICE_TYPE::NV_ENC_DEVICE_TYPE_DIRECTX;
        open.device = dev.as_raw();
        open.apiVersion = nv::NVENCAPI_VERSION;
        let mut enc = std::ptr::null_mut();
        let st = (api.f.nvEncOpenEncodeSessionEx.unwrap())(&mut open, &mut enc);
        api.enc = enc;
        api.check(st, "OpenEncodeSessionEx (D3D12 device)");
    }
    println!("NVENC session opened on the D3D12 device");
    if args.list {
        list(&api);
        unsafe { (api.f.nvEncDestroyEncoder.unwrap())(api.enc) };
        return;
    }

    let codec = match args.codec.as_str() {
        "hevc" => guid::NV_ENC_CODEC_HEVC_GUID,
        "av1" => guid::NV_ENC_CODEC_AV1_GUID,
        _ => guid::NV_ENC_CODEC_H264_GUID,
    };
    let tuning = match args.tuning.as_str() {
        "hq" => nv::NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_HIGH_QUALITY,
        "ull" => nv::NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY,
        _ => nv::NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_LOW_LATENCY,
    };
    let preset = preset_guid(args.preset);

    let mut preset_cfg: nv::NV_ENC_PRESET_CONFIG = unsafe { zeroed() };
    preset_cfg.version = version::NV_ENC_PRESET_CONFIG_VER;
    preset_cfg.presetCfg.version = version::NV_ENC_CONFIG_VER;
    unsafe {
        api.check(
            (api.f.nvEncGetEncodePresetConfigEx.unwrap())(
                api.enc,
                codec,
                preset,
                tuning,
                &mut preset_cfg,
            ),
            "GetEncodePresetConfigEx",
        );
    }
    let mut cfg = preset_cfg.presetCfg;
    cfg.frameIntervalP = 1;
    cfg.rcParams.rateControlMode = if args.tuning == "hq" {
        nv::NV_ENC_PARAMS_RC_MODE::NV_ENC_PARAMS_RC_VBR
    } else {
        nv::NV_ENC_PARAMS_RC_MODE::NV_ENC_PARAMS_RC_CBR
    };
    cfg.rcParams.averageBitRate = 20_000_000;
    cfg.rcParams.maxBitRate = 20_000_000;

    let mut init: nv::NV_ENC_INITIALIZE_PARAMS = unsafe { zeroed() };
    init.version = version::NV_ENC_INITIALIZE_PARAMS_VER;
    init.encodeGUID = codec;
    init.presetGUID = preset;
    init.encodeWidth = WIDTH;
    init.encodeHeight = HEIGHT;
    init.darWidth = WIDTH;
    init.darHeight = HEIGHT;
    init.frameRateNum = 60;
    init.frameRateDen = 1;
    init.enableEncodeAsync = 0;
    init.enablePTD = 1;
    init.encodeConfig = &mut cfg;
    init.maxEncodeWidth = WIDTH;
    init.maxEncodeHeight = HEIGHT;
    init.tuningInfo = tuning;
    init.bufferFormat = nv::NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_NV12;
    unsafe {
        api.check(
            (api.f.nvEncInitializeEncoder.unwrap())(api.enc, &mut init),
            "InitializeEncoder",
        )
    };

    let nv12_desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        Width: WIDTH as u64,
        Height: HEIGHT,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
        Flags: D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS,
        ..Default::default()
    };
    let mut footprints = [D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default(); 2];
    let mut rows = [0u32; 2];
    let mut row_bytes = [0u64; 2];
    let mut total = 0u64;
    unsafe {
        dev.GetCopyableFootprints(
            &nv12_desc,
            0,
            2,
            0,
            Some(footprints.as_mut_ptr()),
            Some(rows.as_mut_ptr()),
            Some(row_bytes.as_mut_ptr()),
            Some(&mut total),
        );
    }
    let upload_fence: ID3D12Fence = unsafe { dev.CreateFence(0, D3D12_FENCE_FLAG_NONE).unwrap() };
    let event = unsafe { CreateEventW(None, false, false, None).unwrap() };
    let stride = total.next_multiple_of(512);
    let upload = committed(
        &dev,
        D3D12_HEAP_TYPE_UPLOAD,
        buffer_desc(stride * UNIQUE_FRAMES as u64),
        D3D12_RESOURCE_STATE_GENERIC_READ,
    );
    let sources: Vec<ID3D12Resource> = (0..UNIQUE_FRAMES)
        .map(|_| {
            committed(
                &dev,
                D3D12_HEAP_TYPE_DEFAULT,
                nv12_desc,
                D3D12_RESOURCE_STATE_COMMON,
            )
        })
        .collect();
    let inputs: Vec<ID3D12Resource> = if args.gpu_copy {
        (0..OUTPUTS_IN_FLIGHT)
            .map(|_| {
                committed(
                    &dev,
                    D3D12_HEAP_TYPE_DEFAULT,
                    nv12_desc,
                    D3D12_RESOURCE_STATE_COMMON,
                )
            })
            .collect()
    } else {
        sources.clone()
    };
    unsafe {
        let mut p = std::ptr::null_mut();
        upload.Map(0, None, Some(&mut p)).unwrap();
        for k in 0..UNIQUE_FRAMES {
            let base = (p as *mut u8).add(k * stride as usize);
            let (y_fp, uv_fp) = (&footprints[0], &footprints[1]);
            let y = std::slice::from_raw_parts_mut(
                base.add(y_fp.Offset as usize),
                y_fp.Footprint.RowPitch as usize * HEIGHT as usize,
            );
            let uv = std::slice::from_raw_parts_mut(
                base.add(uv_fp.Offset as usize),
                uv_fp.Footprint.RowPitch as usize * (HEIGHT as usize / 2),
            );
            fill_nv12(
                k as u32,
                y,
                y_fp.Footprint.RowPitch as usize,
                uv,
                uv_fp.Footprint.RowPitch as usize,
            );
        }
        upload.Unmap(0, None);

        let queue: ID3D12CommandQueue = dev
            .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                ..Default::default()
            })
            .unwrap();
        let alloc: ID3D12CommandAllocator = dev
            .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)
            .unwrap();
        let list: ID3D12GraphicsCommandList = dev
            .CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &alloc, None)
            .unwrap();
        for (k, tex) in sources.iter().enumerate() {
            for plane in 0..2u32 {
                let mut fp = footprints[plane as usize];
                fp.Offset += k as u64 * stride;
                let dst = D3D12_TEXTURE_COPY_LOCATION {
                    pResource: std::mem::ManuallyDrop::new(Some(tex.clone())),
                    Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                        SubresourceIndex: plane,
                    },
                };
                let src = D3D12_TEXTURE_COPY_LOCATION {
                    pResource: std::mem::ManuallyDrop::new(Some(upload.clone())),
                    Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                        PlacedFootprint: fp,
                    },
                };
                list.CopyTextureRegion(&dst, 0, 0, 0, &src, None);
            }
        }
        list.Close().unwrap();
        queue.ExecuteCommandLists(&[Some(list.cast::<ID3D12CommandList>().unwrap())]);
        queue.Signal(&upload_fence, 1).unwrap();
    }
    wait_fence(&upload_fence, event, 1);
    drop(upload);
    let copy_lists: Vec<(ID3D12CommandAllocator, ID3D12GraphicsCommandList)> = if args.gpu_copy {
        (0..OUTPUTS_IN_FLIGHT)
            .map(|k| unsafe {
                let a: ID3D12CommandAllocator = dev
                    .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)
                    .unwrap();
                let l: ID3D12GraphicsCommandList = dev
                    .CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &a, None)
                    .unwrap();
                l.CopyResource(&inputs[k], &sources[k % UNIQUE_FRAMES]);
                l.Close().unwrap();
                (a, l)
            })
            .collect()
    } else {
        Vec::new()
    };
    let in_fence: ID3D12Fence = unsafe { dev.CreateFence(0, D3D12_FENCE_FLAG_NONE).unwrap() };
    let copy_queue: ID3D12CommandQueue = unsafe {
        dev.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
            Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
            ..Default::default()
        })
        .unwrap()
    };
    let fence: ID3D12Fence = unsafe { dev.CreateFence(0, D3D12_FENCE_FLAG_NONE).unwrap() };

    let mut upload_fence_point = nv::NV_ENC_FENCE_POINT_D3D12 {
        version: version::NV_ENC_FENCE_POINT_D3D12_VER,
        reserved: 0,
        pFence: fence.as_raw(),
        waitValue: 0,
        signalValue: 0,
        _bitfield_align_1: [],
        _bitfield_1: Default::default(),
        reserved1: [0; 7],
    };

    let mut mapped_in: Vec<nv::NV_ENC_INPUT_PTR> = Vec::new();
    let mut registered_in: Vec<nv::NV_ENC_REGISTERED_PTR> = Vec::new();
    for tex in &inputs {
        unsafe {
            let mut reg: nv::NV_ENC_REGISTER_RESOURCE = zeroed();
            reg.version = version::NV_ENC_REGISTER_RESOURCE_VER;
            reg.resourceType = nv::NV_ENC_INPUT_RESOURCE_TYPE::NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX;
            reg.width = WIDTH;
            reg.height = HEIGHT;
            reg.pitch = 0;
            reg.resourceToRegister = tex.as_raw();
            reg.bufferFormat = nv::NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_NV12;
            reg.bufferUsage = nv::NV_ENC_BUFFER_USAGE::NV_ENC_INPUT_IMAGE;
            reg.pInputFencePoint = &mut upload_fence_point;
            api.check(
                (api.f.nvEncRegisterResource.unwrap())(api.enc, &mut reg),
                "RegisterResource (input)",
            );
            registered_in.push(reg.registeredResource);
            let mut map: nv::NV_ENC_MAP_INPUT_RESOURCE = zeroed();
            map.version = version::NV_ENC_MAP_INPUT_RESOURCE_VER;
            map.registeredResource = reg.registeredResource;
            api.check(
                (api.f.nvEncMapInputResource.unwrap())(api.enc, &mut map),
                "MapInputResource (input)",
            );
            mapped_in.push(map.mappedResource);
        }
    }

    let out_size = (WIDTH * HEIGHT * 3) as u64;
    let out_bufs: Vec<ID3D12Resource> = (0..OUTPUTS_IN_FLIGHT)
        .map(|_| {
            committed(
                &dev,
                D3D12_HEAP_TYPE_READBACK,
                buffer_desc(out_size),
                D3D12_RESOURCE_STATE_COPY_DEST,
            )
        })
        .collect();
    let mut out_res: Vec<Box<nv::NV_ENC_OUTPUT_RESOURCE_D3D12>> = Vec::new();
    for b in &out_bufs {
        unsafe {
            let mut reg: nv::NV_ENC_REGISTER_RESOURCE = zeroed();
            reg.version = version::NV_ENC_REGISTER_RESOURCE_VER;
            reg.resourceType = nv::NV_ENC_INPUT_RESOURCE_TYPE::NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX;
            reg.width = out_size as u32;
            reg.height = 1;
            reg.pitch = 0;
            reg.resourceToRegister = b.as_raw();
            reg.bufferFormat = nv::NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_U8;
            reg.bufferUsage = nv::NV_ENC_BUFFER_USAGE::NV_ENC_OUTPUT_BITSTREAM;
            api.check(
                (api.f.nvEncRegisterResource.unwrap())(api.enc, &mut reg),
                "RegisterResource (output)",
            );
            let mut map: nv::NV_ENC_MAP_INPUT_RESOURCE = zeroed();
            map.version = version::NV_ENC_MAP_INPUT_RESOURCE_VER;
            map.registeredResource = reg.registeredResource;
            api.check(
                (api.f.nvEncMapInputResource.unwrap())(api.enc, &mut map),
                "MapInputResource (output)",
            );
            let mut o: Box<nv::NV_ENC_OUTPUT_RESOURCE_D3D12> = Box::new(zeroed());
            o.version = version::NV_ENC_OUTPUT_RESOURCE_D3D12_VER;
            o.pOutputBuffer = map.mappedResource;
            o.outputFencePoint.version = version::NV_ENC_FENCE_POINT_D3D12_VER;
            o.outputFencePoint.pFence = fence.as_raw();
            o.outputFencePoint.set_bSignal(1);
            out_res.push(o);
        }
    }

    let mut input_res: Box<nv::NV_ENC_INPUT_RESOURCE_D3D12> = unsafe { Box::new(zeroed()) };
    input_res.version = version::NV_ENC_INPUT_RESOURCE_D3D12_VER;
    input_res.inputFencePoint.version = version::NV_ENC_FENCE_POINT_D3D12_VER;
    input_res.inputFencePoint.pFence = fence.as_raw();

    let mut out_file = args.out.as_ref().map(|p| std::fs::File::create(p).unwrap());
    let mut bytes = 0u64;
    let mut collect = |api: &Api,
                       o: usize,
                       out_res: &mut Vec<Box<nv::NV_ENC_OUTPUT_RESOURCE_D3D12>>,
                       bytes: &mut u64| unsafe {
        let mut lock: nv::NV_ENC_LOCK_BITSTREAM = zeroed();
        lock.version = version::NV_ENC_LOCK_BITSTREAM_VER;
        lock.outputBitstream = &mut *out_res[o] as *mut _ as *mut c_void;
        api.check(
            (api.f.nvEncLockBitstream.unwrap())(api.enc, &mut lock),
            "LockBitstream",
        );
        *bytes += lock.bitstreamSizeInBytes as u64;
        if let Some(f) = out_file.as_mut() {
            f.write_all(std::slice::from_raw_parts(
                lock.bitstreamBufferPtr as *const u8,
                lock.bitstreamSizeInBytes as usize,
            ))
            .unwrap();
        }
        api.check(
            (api.f.nvEncUnlockBitstream.unwrap())(api.enc, lock.outputBitstream),
            "UnlockBitstream",
        );
    };

    let start = Instant::now();
    let mut first_output = None;
    for i in 0..args.frames {
        let o = i % OUTPUTS_IN_FLIGHT;
        if i >= OUTPUTS_IN_FLIGHT {
            wait_fence(&fence, event, (i - OUTPUTS_IN_FLIGHT + 1) as u64);
            collect(&api, o, &mut out_res, &mut bytes);
            first_output.get_or_insert_with(|| start.elapsed());
        }
        out_res[o].outputFencePoint.signalValue = (i + 1) as u64;
        let slot = i % mapped_in.len();
        input_res.pInputBuffer = mapped_in[slot];
        if args.gpu_copy {
            unsafe {
                copy_queue.ExecuteCommandLists(&[Some(
                    copy_lists[slot].1.cast::<ID3D12CommandList>().unwrap(),
                )]);
                copy_queue.Signal(&in_fence, (i + 1) as u64).unwrap();
            }
            input_res.inputFencePoint.pFence = in_fence.as_raw();
            input_res.inputFencePoint.waitValue = (i + 1) as u64;
            input_res.inputFencePoint.set_bWait(1);
        }
        unsafe {
            let mut pic: nv::NV_ENC_PIC_PARAMS = zeroed();
            pic.version = version::NV_ENC_PIC_PARAMS_VER;
            pic.inputWidth = WIDTH;
            pic.inputHeight = HEIGHT;
            pic.inputBuffer = &mut *input_res as *mut _ as *mut c_void;
            pic.outputBitstream = &mut *out_res[o] as *mut _ as *mut c_void;
            pic.bufferFmt = nv::NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_NV12;
            pic.pictureStruct = nv::NV_ENC_PIC_STRUCT::NV_ENC_PIC_STRUCT_FRAME;
            pic.inputTimeStamp = i as u64;
            let st = (api.f.nvEncEncodePicture.unwrap())(api.enc, &mut pic);
            if st != nv::NVENCSTATUS::NV_ENC_ERR_NEED_MORE_INPUT {
                api.check(st, "EncodePicture");
            }
        }
    }
    unsafe {
        let mut eos: nv::NV_ENC_PIC_PARAMS = zeroed();
        eos.version = version::NV_ENC_PIC_PARAMS_VER;
        eos.encodePicFlags = nv::NV_ENC_PIC_FLAGS::NV_ENC_PIC_FLAG_EOS as u32;
        api.check(
            (api.f.nvEncEncodePicture.unwrap())(api.enc, &mut eos),
            "EncodePicture (EOS)",
        );
    }
    let tail = args.frames.min(OUTPUTS_IN_FLIGHT);
    for j in args.frames - tail..args.frames {
        wait_fence(&fence, event, (j + 1) as u64);
        collect(&api, j % OUTPUTS_IN_FLIGHT, &mut out_res, &mut bytes);
    }
    let elapsed = start.elapsed().as_secs_f64();
    println!(
        "{} P{} tuning={} gpu_copy_with_fence_wait={} frames={} elapsed={:.3}s fps={:.1} output={:.2} MB ({:.1} Mbit/s at 60 fps) first_output_after={:.1} ms",
        codec_name(&codec),
        args.preset,
        args.tuning,
        args.gpu_copy,
        args.frames,
        elapsed,
        args.frames as f64 / elapsed,
        bytes as f64 / 1e6,
        bytes as f64 * 8.0 / args.frames as f64 * 60.0 / 1e6,
        first_output
            .map(|d| d.as_secs_f64() * 1e3)
            .unwrap_or(f64::NAN),
    );
    unsafe { (api.f.nvEncDestroyEncoder.unwrap())(api.enc) };
}
