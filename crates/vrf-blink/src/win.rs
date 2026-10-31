use std::sync::OnceLock;

use cef::{CommandLine, ImplCommandLine};
use vrf::gpu::Gpu;
use vrf::gpu::d3d12::Luid;
use windows::Win32::Media::timeBeginPeriod;
use windows::Win32::System::Threading::{
    GetCurrentProcess, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
    PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
    PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling, SetProcessInformation,
};

static ADAPTER: OnceLock<Option<Luid>> = OnceLock::new();

fn adapter() -> Option<Luid> {
    *ADAPTER.get_or_init(|| match std::env::var("VRF_ADAPTER_LUID") {
        Ok(text) => {
            let (high, low) = text.split_once(',')?;
            Some(Luid {
                high: high.parse().ok()?,
                low: low.parse().ok()?,
            })
        }
        Err(_) => Gpu::default_luid().ok(),
    })
}

/// Chromium picks the integrated GPU on a machine that has one, and then its shared handles do
/// not open on the discrete adapter the encoder runs on. The browser names the adapter and
/// every child inherits the switch.
pub fn append_adapter(command_line: &mut CommandLine) {
    if let Some(luid) = adapter() {
        command_line.append_switch_with_value(
            Some(&"use-adapter-luid".into()),
            Some(&luid.to_string().as_str().into()),
        );
    }
}

/// The GPU process sleeps on the Windows timer tick, 15.6 ms by default, between a begin frame
/// and its delivery. A 1 ms tick lifts lock-step from 63 to about 208 frames per second
/// (spike 6.6). The browser process needs it for its own short waits.
pub fn raise_timer_resolution() {
    unsafe { timeBeginPeriod(1) };
}

/// Windows 11 runs a process without a visible window as EcoQoS, on efficiency cores and with
/// its timer resolution requests ignored. Every CEF process here is windowless.
pub fn opt_out_of_power_throttling() {
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED
            | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
        StateMask: 0,
    };
    let _ = unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            (&raw const state).cast(),
            size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    };
}
