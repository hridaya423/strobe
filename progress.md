# Progress

Plan: `docs/plans/2026-10-29-render-engine.md`. Research: `docs/research/2026-10-29/`.

| Phase | Status | Evidence |
|---|---|---|
| Research and plan | done 2026-10-29 | baseline timings and six research reports in `docs/research/2026-10-29/` |
| 0. Measurement harness and baselines | in progress | 0.1 scaffold `926e94e`; 0.2 corpus `5d1f7a4` (7 launch videos, all render with their own framework); 0.3 runner `6f9d16f` (2 tests); 0.4 scorer `c5ffe1a` (8 tests, pinned to `metrics.py`). 0.5 refs `da09c99`. References and noise floors built for all 7 compositions (every 5th frame, 1.0 GB). Next: 0.6 baseline on an idle machine |
| 1. Spikes (1.6 done in research) | not started | `02-encode.md` |
| 2. Spine on macOS | in progress | 2.1 IOSurface import and 2.2 NV12/P010 convert merged (12 tests; GPU output byte-identical to ffmpeg over all 2^24 colours and all crop parities, P010 included; 0.08 to 0.12 ms per 1080p convert under load). 2.4 VideoToolbox encoder and pool merged, with a deadlock fix `87a3a09` (12 tests; 1080p H.264 240 fps, HEVC 400 fps at load 35 to 48). 2.5 muxer and 2.6 scheduler merged `aed2a3f` (17 + 9 tests; stitched VideoToolbox + x264 file decodes to identical md5s in ffmpeg and AVFoundation) |
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

## In flight (2026-10-29 15:05), Sonnet 5.5 delegates, one worktree each from `da09c99`

| Agent | Scope |
|---|---|
| A | Phase 1 spikes 1.1, 1.2, 1.3, 1.5, 1.7, 1.10 on macOS (`spikes/01-cef-harness`) |
| B | Tasks 2.1 and 2.2: IOSurface import, NV12/P010 convert, CPU oracle |
| C | Task 2.4 and the pool half of 2.3: VideoToolbox encoder, `CVPixelBufferPool` (`crates/vrf-encode`) |
| D | Tasks 2.5 and 2.6: MP4/MOV muxer, ordered scheduler |
| E | win-pc: Rust toolchain, Windows build, spikes R6 (begin-frame delivery) and R7 (D3D import, NVENC) |

Kept for the orchestrator: Task 0.6 and spikes 1.8 and 1.9, which need an idle machine (load average under 2). Task 2.3's GPU half and 2.7 come after B and C merge.
- Noise floors so far: hf-figma-launch FLIP mean 0.00002 and max delta 4; hf-heygen-stripe FLIP mean 0.00018 and max delta 7. HyperFrames is not bit-identical run to run.
- Noise floors (HyperFrames, every 5th frame): figma FLIP 0.00002, max delta 4; stripe 0.00018, max delta 7; spacex 0.00001, max delta 1; hyperframes-launch worst 0.0132, max delta 13, on frames 485 to 495 only (the talking-head video at 16 s; the heatmap shows a faint uniform difference across the video region, so HyperFrames decodes the same source video frame slightly differently between runs). The other 296 frames average 0.00009. Gate rule to apply at Gates B and C: a frame passes if it meets the §2 bars or sits within its own noise floor plus margin, whichever is looser (contract item 25), because no engine can beat a reference that misses the bar against itself.
- Surprise: Remotion's CLI rejects an image-sequence output path with a dot anywhere in it (this repo is under `~/.t3`). `refs` renders into the system temp folder and moves the kept frames.
- Integration notes from the Task 2.5/2.6 review: the scheduler keeps a clone of the last released frame for `Repeat`, so the `CVPixelBufferPool` needs one buffer more than the reorder window plus encoder frames in flight. The muxer's faststart rewrites the whole file once in `finish`; measure that in Task 2.7.
- Remotion noise floors: transitions-video FLIP 0.00002, max delta 46 (5 frames above 8); 4-0-trailer FLIP 0.00002, max delta 255 on single pixels in 16 frames; github-unwrapped worst FLIP 0.0025 and 0.39% of pixels over 8 at frames 1525 to 1600 (the rocket's edges move by a sub-pixel amount between runs; heatmap checked). Remotion is not bit-stable run to run either, so the per-frame noise-floor gate rule applies to it too.
- Review of Task 2.4: the VideoToolbox output callback blocked on a bounded channel, so a caller that drained late deadlocked the pool. Reproduced with a 300-frame test (it hung), fixed with an unbounded channel, test passes. Accepted from the agent: `flush` added to the `Encoder` trait (call it before `finish`), a session counter capped at `RecommendedParallelizationLimit` (1 on this Mac), and H.264 High profile.
- Review of Tasks 2.1 and 2.2, follow-ups for Task 2.3: (1) `Converter::encode` allocates a uniform buffer and a bind group per call; cache bind groups per pool IOSurface and pass the rect through a dynamic offset or immediates. (2) The shader loads each source pixel twice (luma, then chroma); share the loads if 2.7 shows the convert matters. (3) Odd visible sizes are rejected; contract item 1 says pad to even, so decide the pad (edge replicate or black) against REF. (4) The storage formats need `TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES`; check D3D12 and Vulkan support for `R8Uint`/`Rg8Uint` storage in Phase 6.
