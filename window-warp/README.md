# window-warp

Captures a window on Windows with [`scap`](https://crates.io/crates/scap) (Windows Graphics
Capture) and warps every frame from a rectangle onto a disk on the GPU with Direct3D 11.

The warp wraps the window around the disk:

| source                    | disk                                     |
| ------------------------- | ---------------------------------------- |
| x (left → right)          | angle, clockwise from `--start-angle`    |
| y (top row)               | outer rim                                |
| y (bottom row)            | inner hole (`--inner-radius`)            |

Every frame goes straight from the capture buffer into a dynamic BGRA texture and is resolved
by a pixel shader (`src/shaders/warp.hlsl`), so the whole polar mapping runs on the GPU. The
shader supersamples each output pixel (`--supersample`, 2×2 by default) because the mapping
compresses the source heavily near the hole.

## Build

Requires Windows 10 1903+ and the MSVC toolchain:

```
rustup target add x86_64-pc-windows-msvc
cargo build --release
```

## Use

```
# see what can be captured
window-warp --list

# wrap the Ableton Live window onto a 1080x1080 disk
window-warp --window "Ableton Live" --size 1080

# half circle, no hole, borderless always-on-top output
window-warp --window "Ableton Live" --angle-span 180 --inner-radius 0 --overlay
```

Useful flags: `--display` (capture the main monitor instead of a window), `--fps`, `--cursor`,
`--outer-radius`, `--counter-clockwise`.

### Keys

| key         | action                          |
| ----------- | ------------------------------- |
| `←` / `→`   | rotate the wrap by 2°           |
| `↑` / `↓`   | grow / shrink the inner hole    |
| `PgUp`/`PgDn` | more / fewer samples per pixel |
| `space`     | freeze the last captured frame  |
| `esc`       | quit                            |

## Output frame format

There is no CPU-side output framebuffer: the warp pixel shader writes directly into the
swapchain back buffer, which is

- `DXGI_FORMAT_B8G8R8A8_UNORM` (8 bits per channel, byte order B, G, R, A), full range sRGB,
- alpha always written as `1.0` (`DXGI_ALPHA_MODE_IGNORE`),
- `--size` x `--size` pixels (client area, so it follows window resizes),
- GPU resident only: `D3D11_USAGE_DEFAULT`, no `CPU_ACCESS_*` flag, and its row pitch is
  chosen by the driver, so it is not a tightly packed `width * 4` buffer.

Feeding an H.264 encoder therefore needs an extra step: render the same draw into an offscreen
`D3D11_BIND_RENDER_TARGET` texture, convert BGRA to NV12 (a compute shader, the Video Processor
of `ID3D11VideoContext`, or the encoder's own input conversion), and hand the resulting texture
to an encoder that accepts D3D11 surfaces (Media Foundation H.264 MFT / Sink Writer, NVENC,
AMF). Reading the pixels back to system memory only to encode them is what to avoid; keeping
the frame on the GPU all the way to the encoder is the reason the warp is done in a shader.

## Notes

`scap` 0.0.8 does not compile against `windows-capture` 1.5, so that transitive dependency is
pinned to 1.4.4 in `Cargo.toml`.
