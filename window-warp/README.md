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

The preview has no CPU-side framebuffer: the warp pixel shader writes directly into the
swapchain back buffer, which is

- `DXGI_FORMAT_B8G8R8A8_UNORM` (8 bits per channel, byte order B, G, R, A), full range sRGB,
- alpha always written as `1.0` (`DXGI_ALPHA_MODE_IGNORE`),
- `--size` x `--size` pixels (client area, so it follows window resizes),
- GPU resident only: `D3D11_USAGE_DEFAULT`, no `CPU_ACCESS_*` flag, and its row pitch is
  chosen by the driver, so it is not a tightly packed `width * 4` buffer.

## H.264 streaming

`--stream udp://HOST:PORT` encodes the warped frames and sends them over the network. Nothing
but the compressed bitstream is copied to system memory:

```
warp pixel shader -> offscreen BGRA render target (--stream-size, GPU)
                  -> NV12 via ID3D11VideoContext VideoProcessorBlt (GPU)
                  -> Media Foundation H.264 MFT, hardware first (GPU)
                  -> IMFMediaBuffer::Lock, a few kB per frame     (CPU)
                  -> MPEG-TS packets -> UDP datagrams of 7 x 188 bytes
```

The preview window keeps its own swapchain, so resizing or moving it does not change the
encoded resolution.

```
# 1080x1080 disk, multicast to the LAN, plays with `ffplay udp://239.0.0.1:5004`
window-warp --window "Ableton Live" --size 1080 --stream udp://239.0.0.1:5004

# unicast to one machine, 4 Mbit/s, key frame every second
window-warp --window "Ableton Live" --stream udp://192.168.1.42:5004 --bitrate 4000000 --gop 60
```

Stream flags: `--stream-size` (encoded side length, defaults to `--size`), `--bitrate` (CBR,
8 Mbit/s by default), `--gop` (frames between key frames, two seconds by default),
`--multicast-ttl`. The bitstream is Annex B H.264 High profile, low latency (no B frames), and
the transport stream carries one program with the video on PID `0x100`.

## Notes

`scap` 0.0.8 does not compile against `windows-capture` 1.5, so that transitive dependency is
pinned to 1.4.4 in `Cargo.toml`.
