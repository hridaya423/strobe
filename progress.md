# Progress

Plan: `docs/plans/2026-10-29-render-engine.md`. Research: `docs/research/2026-10-29/`.

| Phase | Status | Evidence |
|---|---|---|
| Research and plan | done 2026-10-29 | baseline timings and six research reports in `docs/research/2026-10-29/` |
| 0. Measurement harness and baselines | in progress | 0.1 scaffold `926e94e`; 0.2 corpus `5d1f7a4` (7 launch videos, all render with their own framework); 0.3 runner `6f9d16f` (2 tests); 0.4 scorer `c5ffe1a` (8 tests, pinned to `metrics.py`). Next: 0.5 references, 0.6 baseline |
| 1. Spikes (1.6 done in research) | 1.1, 1.2, 1.3, 1.5, 1.7, 1.10 measured on branch `t3/cef-render-engine-phase1-spikes`; 1.4, 1.8, 1.9 not started | 1.1 `2037ca5` pass; 1.2 `5fe63b6` pass (1 begin frame); 1.3 `849d386` fail (0.77 to 1.05 ms vs 0.3 ms, patch 4 needed); 1.7 `4211f89` fail (clocks leak); 1.10 `57120e1` pass; 1.5 `56c0d6f`. Docs in `docs/spikes/`. Timings taken at load 2 to 33, provisional |
| 2. Spine on macOS | not started | |
| 3. Blink path on macOS, Gate B | not started | |
| 4. Throughput, Gate C | not started | |
| 5. Media | not started | |
| 6. Windows and Linux | not started | |
| 7. CEF patch set | not started | |
| 8. Native fast path (gated) | not opened | |

## Baselines measured during research (M4 Pro, 1080p, 300 frames, `bench-gsap-1080p.html`)

- HyperFrames 0.8.216: 7.0 to 9.8 s wall.
- Kinetograph 0.2.5: 2.5 s wall at 1 worker, 2.9 to 3.3 s at 4 workers. SSIM 0.986 vs HyperFrames frames.
- VideoToolbox ceilings at 1080p: H.264 230 fps, HEVC 370 fps, ProRes 422 936 fps.

## Scope (user, 2026-10-29)

- Run Phases 0 to 7. Phase 8 stays gated.
- Windows work runs on `win-pc` (ssh alias): RTX 4070 Ti, driver 617.42, Git present, no Rust yet.
- Linux testing is deferred. The user will decide it later.
- Phase 7: the user decides how to do the Chromium build (about 100 GB, more than this Mac's free disk) when the phase begins. Do not skip the phase.

## Notes

- 2026-10-29 13:27: load average 28 (other node processes and Chrome). `vrf-bench run` refuses at 2 or more, so timing waits for an idle machine.
- 2026-10-29: the user replaced the hand-written corpus with real launch videos (3 or 4 HyperFrames launches and the top open-source Remotion ones). Plan Task 0.2 is rewritten to match.
- Kinetograph 0.2.5 failed both smoke renders it was given: `hf-spacex-launch` (GSAP targets resolve to null, then a 30 s `delayRender` timeout) and `rm-transitions-video` ("radius must be a positive number" at frame 252). Task 0.6 records every failure.
- Surprise: Remotion 4.0.240 unpacked a truncated `chrome-headless-shell` on this machine (Node 26.7). The zip is valid, and `ditto` unpacks it correctly. `fetch-external.sh` checks the binary and re-extracts it.
- Surprise: Remotion 4.0.169 and 4.0.60 use the system Google Chrome when present. All Remotion runs therefore pass `--browser-executable` with a pinned `chrome-headless-shell` 156.0.8078.73.
- `metrics.py` computes tail fractions in float32, which counts some deltas of exactly 8 as over 8. The scorer uses integer deltas; see plan Task 0.4.

- 2026-10-29: Spike 1.5. Each raster mode is repeatable (5 runs, 0 differing frames). OSR CPU raster is not equal to headless-shell CPU: 0 of 300 frames hash-equal, differences only in the canvas and WebGL tiles (worst max delta 74). GPU raster differs from CPU raster on up to 23.8% of pixels (max delta 136). macOS headless-shell rejects BeginFrameControl, so the golden reference uses `Page.captureScreenshot`.
- 2026-10-29: Spike 1.3: wgpu 30 forbids R8Uint and Rg8Uint storage textures. Tasks 2.2 and 2.3 write NV12 with two render passes into R8Unorm and Rg8Unorm. A wgpu/Metal buffer copy at tiny offsets returned wrong bytes; readbacks use 256-byte spacing.
- 2026-10-29: Spike 1.3: import, copy and convert cost 0.77 to 1.05 ms at 1080p, over the 0.3 ms line. CEF patch 4 (deferred release) is required. Cache imported textures by IOSurface ID. VideoToolbox calls at 4K blocked about 6 ms, so encoder calls go on a submit thread.
- 2026-10-29: Spike 1.2: one begin frame after a seek showed the seek in 900 of 900 frames. Task 3.2 sends one begin frame, then waits for the delivery or the watchdog. No delivery means no damage.
- 2026-10-29: Spike 1.7: all page clocks follow the wall clock. Patch 1 would fix rAF and CSS animation only. Adapters need a virtual clock for `performance.now` and `Date.now`. Test `Emulation.setVirtualTimePolicy` first.
- Surprise: `cef-dll-sys` needs cmake and ninja on PATH. A bundle builder must `rm` a signed binary before `cp`, or the copy is killed with SIGKILL.
