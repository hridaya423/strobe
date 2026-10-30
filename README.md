<h1 align="center">
  <img src="assets/readme/banner.png" alt="Strobe" width="100%">
</h1>

<p align="center">
  <b>Render your Remotion and HyperFrames projects 2.4× to 5.2× faster. Change no code.</b>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Windows-0B2A6B?style=flat-square" alt="Platform: macOS and Windows">
  <img src="https://img.shields.io/badge/rust-1.99.1-0B2A6B?style=flat-square" alt="Rust 1.99.1">
  <img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-0B2A6B?style=flat-square" alt="License: MIT OR Apache-2.0">
</p>

<p align="center">
  <a href="#the-numbers">The numbers</a> ·
  <a href="#why-it-is-faster">Why it is faster</a> ·
  <a href="#build-it">Build it</a> ·
  <a href="#use-it">Use it</a> ·
  <a href="#what-does-not-work-yet">What does not work yet</a>
</p>

---

Strobe is a render engine for HTML video. You point it at a Remotion project or a HyperFrames project, and it writes the MP4. You keep your compositions, your props and your `delayRender` calls.

In Remotion and HyperFrames, each frame leaves the GPU, becomes a JPEG or a PNG, and goes through FFmpeg. Strobe keeps the frame on the GPU until the hardware encoder has it.

> [!NOTE]
> The brand is Strobe. The binary is still called `vrf`, from the working name of the repository.

```sh
vrf render ./my-remotion-app --composition Main -o out.mp4
vrf render ./my-hyperframes-video -o out.mp4
```

## The numbers

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/readme/speed-dark.png">
    <img src="assets/readme/speed-light.png" alt="Wall time on seven launch videos. Strobe is 2.4 to 5.2 times faster than Remotion or HyperFrames." width="100%">
  </picture>
</p>

| Launch video | Rival | Rival time | Strobe time | Speedup |
|---|---|---:|---:|---:|
| `github-unwrapped` | Remotion 4.0.539 | 44.28 s | 8.49 s | **5.2×** |
| `transitions-video` | Remotion 4.0.539 | 13.79 s | 3.01 s | **4.6×** |
| `spacex-launch` | HyperFrames 0.8.216 | 15.90 s | 4.46 s | **3.6×** |
| `figma-launch` | HyperFrames 0.8.216 | 30.69 s | 8.71 s | **3.5×** |
| `heygen-stripe` | HyperFrames 0.8.216 | 23.02 s | 7.63 s | **3.0×** |
| `hyperframes-launch` | HyperFrames 0.8.216 | 26.18 s | 9.25 s | **2.8×** |
| `4-0-trailer` | Remotion 4.0.539 | 38.94 s | 16.28 s | **2.4×** |

The times are wall clock, from the command to the finished file. The machine is an Apple M4 Mac mini with 10 cores and 16 GiB of memory. The rival times are the median of five runs. The Strobe times on the HyperFrames videos come from one run each, so treat those four rows with care.

Seven videos are not a benchmark suite. The repository also holds 26 synthetic projects that each stress one axis: canvas, text, images, 4K, WebGL, embedded video and more. Run them and send us the numbers from your machine.

<details>
<summary><b>Run the benchmark yourself</b></summary>

```sh
bench/fetch-external.sh
cargo build --release -p vrf-cli -p vrf-bench
target/release/vrf-bench run --engine vrf,remotion,hyperframes --corpus all --runs 5
```

The bench refuses to start while the load average is 2 or higher. It waits up to 600 seconds for the machine to go idle. Close other apps first.

</details>

## Why it is faster

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/readme/architecture-dark.png">
    <img src="assets/readme/architecture-light.png" alt="Remotion and HyperFrames screenshot each frame to a JPEG or PNG and pipe it to FFmpeg. Strobe sends each frame from CEF through IOSurface and a wgpu convert into VideoToolbox, all on the GPU." width="100%">
  </picture>
</p>

Remotion and HyperFrames drive Chrome through DevTools. For each frame, they ask for a screenshot. Chrome reads the pixels back from the GPU and compresses them on the CPU. FFmpeg then decompresses the image and encodes it again. That is a lot of work for a frame that was already on the GPU.

Strobe embeds CEF 156, the Chromium Embedded Framework, with off-screen rendering. These are the parts that matter:

- **One begin frame per seek.** Strobe drives the compositor in lock step. Each seek gets exactly one begin frame, so no frame is dropped or painted twice.
- **A virtual clock.** A small script replaces `Date.now`, `performance.now`, `requestAnimationFrame` and `document.timeline`. Your animations see the frame time, not the wall time.
- **Zero CPU readbacks on macOS.** The painted frame arrives as an IOSurface. A wgpu compute shader converts it to NV12, and VideoToolbox encodes it. On Windows, the same path goes through D3D12 and NVENC.
- **No work on frames that do not change.** If the page reports no damage, Strobe repeats the last frame. Strobe does not paint a held title card again.
- **Parallel views.** Strobe measures your machine and splits the frames across several views. `--views` and `--run` override the measurement.
- **A second H.264 lane on macOS.** On the M4 Mac mini, VideoToolbox recommends one encoder session. Strobe runs libx264 beside it on the spare cores.
- **A warm daemon.** `vrf daemon` keeps CEF loaded between renders, so the next render skips the startup cost.

Stock CEF cannot decode H.264 in a `<video>` element. Strobe decodes embedded video with VideoToolbox instead. If a clip does not match the framework's own decoder exactly, Strobe falls back to that decoder for the clip.

## Build it

There are no release binaries yet. Build from source.

You need these tools:

- Rust 1.99.1. The `rust-toolchain.toml` file pins the version.
- cmake and ninja, to build CEF.
- Node, plus the dependencies of your project.
- x264 for the second H.264 lane: `brew install x264` on macOS. Set `VRF_LIBX264` if the library is not at `/opt/homebrew/lib/libx264.dylib`.

```sh
git clone <repository-url> strobe
cd strobe
cargo build --release -p vrf-cli
```

The binary is `target/release/vrf`.

## Use it

```sh
vrf render ./my-remotion-app --composition Main -o out.mp4 --props '{"title":"Hi"}'
vrf render ./my-hyperframes-video -o out.mov --codec prores --fps 60
vrf render ./my-remotion-app --composition Main -o clip.mp4 --frames 0-89
```

For a queue of renders, start the daemon once:

```sh
vrf daemon &
vrf render ./a --composition Main -o a.mp4
vrf render ./b --composition Main -o b.mp4
```

Projects that use WebGL render on SwiftShader. They need a daemon of their own, which you start with `VRF_SOFTWARE_WEBGL=1 vrf daemon`.

| Flag | What it does |
|---|---|
| `--composition` | Selects the Remotion composition. A Remotion project requires it. |
| `--codec` | Selects `h264` (the default), `hevc` or `prores`. |
| `--mode` | Selects `reference` or `fast`. See [Pixels](#pixels). |
| `--frames` | Renders one frame, or a range such as `0-89`. |
| `--props` | Passes input props as JSON, or as the path of a JSON file. |
| `--fps` | Sets the frame rate of a HyperFrames render. The default is 30. |
| `--offline` | Fails on any http(s) request that `vrf.lock.json` does not pin. |
| `--video` | Selects the decoder for embedded video: `native` (the default) or `remotion`. |
| `--views`, `--run` | Override the measured number of parallel views and the frames per view. |
| `--no-skip` | Renders every frame, including unchanged frames. |
| `--no-turbo` | Encodes H.264 on VideoToolbox alone. |
| `--no-daemon` | Renders in this process, even if a daemon runs. |
| `--json` | Prints a JSON report instead of the summary line. |

`vrf render --help` lists the rest.

### Pixels

Speed is worth nothing if the frames are wrong, so Strobe has two raster modes.

`--mode reference` rasterises on the CPU. It gives the same pixels as the `chrome-headless-shell` that Remotion ships. Remotion projects use this mode by default.

`--mode fast` rasterises on the GPU, as HyperFrames does. It is up to 30% quicker on pages where raster is the bottleneck. Blended fills and gradients can land one level away from the Remotion pixels. HyperFrames projects use this mode by default.

## What does not work yet

We would rather you read this here than find it in production.

- **Linux.** Strobe runs on macOS and Windows. Linux is not done.
- **Text on Windows.** Windows draws glyph edges with DirectWrite, and macOS uses CoreText. On Windows, `github-unwrapped` and `4-0-trailer` do not pass our 10% fidelity margin yet. `progress.md` tracks the details.
- **Embedded video on Windows.** The `native` decoder uses VideoToolbox, so it works only on macOS.
- **Release builds.** There is no installer and no prebuilt binary.
- **The numbers.** Seven launch videos on one Mac mini are a start, not proof. If Strobe is slower on your project, open an issue with the project. We want to see it.

## License

Strobe is available under the MIT license or the Apache License 2.0, at your option.
