# Encoder ceilings, Hridyas-mini, 2026-10-29 22:29

Apple M4, macOS 27.0.1. spine_bench: GPU convert, encode and mux, frames already in GPU memory, no page. Median of 5 after one warm-up, load average under 2 before each run.

| Case | Median fps | Runs |
|---|---|---|
| H.264 1080p | 228.7 |  228.7 223.9 166.5 238.7 238.9 |
| HEVC 1080p | 384.7 |  384.7 384.4 385.1 384.4 384.8 |
| H.264 1080p turbo | 480.6 |  480.5 480.7 480.6 480.4 480.8 |
| H.264 4K | 63.9 |  63.9 64.0 63.9 63.9 63.9 |
| HEVC 4K | 108.0 |  108.0 107.9 108.0 108.0 107.9 |
| H.264 4K turbo | 127.3 |  127.1 127.3 127.4 127.2 127.6 |
| ProRes 422 1080p | 1188.0 |  1188.4 1187.3 1189.2 1186.9 1188.0 |
