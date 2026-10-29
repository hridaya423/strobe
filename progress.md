# Progress

Plan: `docs/plans/2026-10-29-render-engine.md`. Research: `docs/research/2026-10-29/`.

| Phase | Status | Evidence |
|---|---|---|
| Research and plan | done 2026-10-29 | baseline timings and six research reports in `docs/research/2026-10-29/` |
| 0. Measurement harness and baselines | in progress | 0.1 scaffold builds (`cargo build --workspace`) |
| 1. Spikes (1.6 done in research) | not started | `02-encode.md` |
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
