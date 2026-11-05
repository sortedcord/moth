# moth (POC)

A native YouTube desktop client.

Linux + Wayland only. UI in [gpui](https://gpui.rs) (Vulkan via wgpu); video via GStreamer, with
`waylandsink` attaching a `wl_subsurface` to the gpui window so hardware-decoded frames (VA-API
dmabufs) go straight to the compositor.

## Performance

![moth vs YouTube in Helium: 6.5× less CPU, 8.1× less memory, 15× fewer processes](docs/benchmark.svg)

Same video, codec and resolution in both, hardware-decoded via VA-API, averaged over 40 s of playback.
CPU, memory and GPU are measured over each app's whole process tree; power is the CPU package's
draw above idle (RAPL), which is noisy to about ±0.2 W. moth still uses mock data, so it doesn't yet
pay for loading a real watch page the way the browser does.

## Setup (Arch)

```sh
sudo pacman -S rustup gst-plugins-good gst-plugins-bad gst-plugins-bad-libs gst-plugin-va \
  gst-plugin-pipewire gst-libav yt-dlp deno intel-media-driver vulkan-intel \
  wayland libxkbcommon fontconfig freetype2 cmake clang
rustup default stable
./scripts/fetch-vendor.sh      # clones gpui (Zed) at the pinned rev + applies patches/
cargo run --release
```

`MOTH_OPEN=<index>` opens that video at startup (dev aid).

Keys on the watch page: `space`/`k` play-pause, `j`/`l` ±10s, `←`/`→` ±5s, `esc` back.

## Layout

- `src/main.rs`: gpui views (home grid, watch page) and the video canvas that positions the subsurface
- `src/player.rs`: yt-dlp stream resolution and the GStreamer pipeline (2× `uridecodebin3` → `waylandsink` / `pipewiresink`)
- `src/data.rs`: hardcoded mock data
- `patches/`: local patches to vendored gpui (currently: Vulkan-only wgpu backend)
