# HyperFrames worker spread (partial)

The bar for a HyperFrames frame is HyperFrames' own MP4 at the worst of 1 to 10 workers. Per project and metric the bar is the maximum over worker counts of the worst-frame value, scored against the 8-worker REF in `bench/refs/<id>/frames` with `vrf-bench score --frames every:5`.

Data: `hyperframes-worker-spread.json`. Scripts to finish it: `bench/hf-worker-spread/`.

Status: partial. Stopped on the orchestrator's instruction that agents do no renders. Nothing here is timed.

| Project | Workers measured | Status |
|---|---|---|
| hf-spacex-launch | 1 to 10 | complete |
| hf-figma-launch | 1 to 9 | missing 10 |
| hf-hyperframes-launch | 1 to 5 | missing 6 to 10 |
| hf-heygen-stripe | 1 to 2 | missing 3 to 10 |
| 13 `synth-*-hf` | none | not started |

## Bar so far (worst frame in brackets)

| Project | FLIP mean | FLIP p99.9 | over 8 | over 32 |
|---|---|---|---|---|
| hf-spacex-launch (complete) | 0.03906 w1 (470) | 0.65292 w6 (125) | 3.495% w2 (460) | 1.754% w10 (460) |
| hf-figma-launch (1 to 9) | 0.05013 w2 (430) | 0.66315 w9 (1270) | 10.61% w7 (350) | 1.019% w4 (430) |
| hf-hyperframes-launch (1 to 5, raw REF) | 0.11723 w4 (115) | 0.52704 w5 (765) | 99.48% w2 (605) | 3.689% w1 (1440) |
| hf-hyperframes-launch (1 to 5, composited) | 0.11723 w4 | 0.55984 w6 | 51.0% w1 | 3.689% w1 |
| hf-heygen-stripe (1 to 2) | 0.05090 w2 (660) | 0.36944 w1 (980) | 1.883% w2 (1050) | 0.243% w2 (775) |

Incomplete rows are lower bounds on the bar. Adding worker counts can only raise them.

These numbers score the MP4, so they include codec error. The 0.0206 to 0.0313 in progress.md for spacex scored HyperFrames' frames before encoding. On the MP4 every worker count from 1 to 10, the 8-worker one included, has worst FLIP mean 0.0391 at frame 470 and over-32 0.0175, so on spacex those two metrics are the MP4's own codec floor against the PNG REF and do not depend on workers. Only p99.9 moves with worker count (0.508 to 0.653). Any renderer whose MP4 is lossier than HyperFrames' file at frame 470 fails mean regardless of raster state.

## Scoring finding: transparent REF

- Every `synth-*-hf` REF frame is RGBA with alpha 0 wherever only the `html, body` background shows (synth-static-hf: alpha 0 on the whole frame 0). The scorer drops alpha and keeps RGB 0, so HyperFrames' own MP4 scores FLIP 0.97 against it.
- Fix used: composite REF over each project's `html, body` background colour (`composite.sh`, read from the project's `index.html`), then score. On synth-static-hf this gives worst FLIP mean 0.0499 for HyperFrames at 3 workers. Raw synth scores are meaningless, as bar or as ours.
- `hf-hyperframes-launch` frames 595 to 620 are almost fully transparent in REF (mean alpha 0.87 of 255). Scored raw as the rule is stated, HyperFrames' bar is 99.5% over 8 there, and our own render lands on the same raw numbers. The composited column is the usable one.
- The other three launch projects have alpha 255 on every frame.

## Ours, reference mode, untimed (partial bar, so indicative only)

Rendered with `vrf render --mode reference` through a daemon on a private socket, cached calibration (4 views, run 200 on the long projects, 2 views on most synth), on a machine at load 20 to 70. Only spacex has a complete bar.

| Project | FLIP mean | p99.9 | over 8 | over 32 | against the bar |
|---|---|---|---|---|---|
| hf-spacex-launch | 0.04389 (1.124x) | 0.55646 (0.85x) | 3.268% (0.94x) | 1.704% (0.97x) | fails FLIP mean only |
| hf-figma-launch | 0.04368 | 0.55667 | 14.92% | 0.647% | over 8 is above the 9-worker bar of 10.61% |
| hf-heygen-stripe | 0.06134 | 0.5174 | 3.722% | 0.491% | bar has 2 of 10 workers |
| hf-hyperframes-launch (composited) | 0.07661 | 0.7315 | 19.96% | 2.564% | raw: 0.1165 / 0.7315 / 99.49% / 2.564% |

All 17 reference renders and scores (including 13 synth) are in the JSON under `ours.reference`. Fast mode was started and stopped before any project finished, so there is nothing for it.

## Rerun

```
cargo build --release -p vrf-cli -p vrf-bench
export OUT=<scratch dir>
for id in $(cat bench/hf-worker-spread/projects.txt); do bench/hf-worker-spread/bar.sh $id; done
bench/hf-worker-spread/ours.sh reference $(cat bench/hf-worker-spread/projects.txt)
bench/hf-worker-spread/ours.sh fast $(cat bench/hf-worker-spread/projects.txt)
python3 -I bench/hf-worker-spread/aggregate.py $OUT bench/results/hyperframes-worker-spread.json
```

`bar.sh` takes `WORKERS="3 4"` to fill gaps, skips worker counts that already have a score, clears the HyperFrames extraction cache by giving each project its own `TMPDIR`, and retries a failed render three times (two first attempts failed under load on spacex w1 and figma w1). `ours.sh` needs a `vrf daemon --mode <mode>` running (set `VRF_SOCKET` to a private path) or it renders in process. `bench/refs`, `bench/engines/node_modules` and `bench/corpus/external` are untracked, so a fresh worktree needs them linked in.
