# AGENTS.md

Strobe is a render engine for HTML-based video. It embeds CEF, renders off screen, and keeps frames on the GPU from the page to the encoder.

The binary is `strobe`. It renders Remotion and HyperFrames projects on macOS and Windows.

## Crates

- `crates/vrf`: muxer, audio mixer, colour handling, GPU frame conversion.
- `crates/vrf-encode`: encoders. VideoToolbox, libx264 and AudioToolbox on macOS. NVENC, D3D12 Video Encode and Media Foundation on Windows.
- `crates/vrf-blink`: the CEF engine, the Remotion and HyperFrames adapters, and the page scripts in `js/`. The Rust side embeds those scripts with `include_str!`.
- `crates/vrf-cli`: the `strobe` binary.
- `crates/vrf-bench`: the benchmark runner and scorer.

Other directories: `bench/` holds corpora, engine pins and scripts. `launch/` is the launch film, a HyperFrames project. `docs/` is local and ignored by git.

## Build and test

- Build: `cargo build --release -p vrf-cli`.
- Unit tests that need no fixtures: `cargo test -p vrf --lib`, `cargo test -p vrf-encode --lib`, `cargo test -p vrf-bench`, `cargo test -p vrf-cli --bin strobe`.
- Integration tests need the external corpus. Run `bench/fetch-external.sh` first.
- The `vrf-blink` and `vrf-cli` integration tests set `harness = false`, because CEF must run on the main thread.
- Page script tests: `npm test` in `crates/vrf-blink/js`. They need chrome-headless-shell (under `bench/engines/browsers`, or `CHROME_HEADLESS_SHELL`) and the packages in `bench/engines/node_modules`.
- The toolchain is pinned in `rust-toolchain.toml`.

## Platforms

- macOS: VideoToolbox. An optional second H.264 lane uses libx264. Set `VRF_LIBX264` to the `.dylib` if it is not in `/opt/homebrew/lib`.
- Windows: NVENC on NVIDIA. D3D12 Video Encode on Intel, H.264 only.
- Linux is unsupported.
- Windows-only code cannot be compiled on a Mac. If you change it, say that it was not built.

## Conventions

- Write no code comments. Public API docs are the exception.
- Commit messages read `area: sentence`, lower case, no trailing period.
- The crates keep the `vrf` name while the binary is `strobe`. Do not rename them.
- `VRF_*` environment variables, `vrf.lock.json` and the `vrf://` scheme are stable names. Do not rename them.
- Commit only the paths you changed. Do not stage the whole tree.

## Benchmarks

Never run benchmarks from an agent session. Timing is done on a dedicated idle machine.
