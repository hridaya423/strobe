# Progress

Plan: `docs/plans/2026-10-29-render-engine.md`. Research: `docs/research/2026-10-29/`.

| Phase | Status | Evidence |
|---|---|---|
| Research and plan | done 2026-10-29 | baseline timings and six research reports in `docs/research/2026-10-29/` |
| 0. Measurement harness and baselines | not started | |
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
