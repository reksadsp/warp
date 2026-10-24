
# unix-warp

`unix-warp` captures a window or display on Linux or macOS, maps the captured
rectangle around a disk with `wgpu`, and can translate mouse movement and
buttons on that disk back to their source coordinates.

On Linux it can also receive the Windows `window-warp` H.264/MPEG-TS UDP stream
and send pointer events back through its TCP sidecar control server.

The renderer runs the polar transform on the GPU. Source `x` maps to the
clockwise angle starting at `--start-angle`; source `y` maps from the outer rim
to the inner hole. The preview supports `Esc` to close it through its native
window controls.

## Development

Use the platform-specific Nix shell from the repository root:

```console
nix develop .#unix-warp
cargo run --manifest-path unix-warp/Cargo.toml -- --list
```

On Linux, the shell provides PipeWire, the XDG desktop portal, Wayland/X11
libraries, and X11 input libraries. On macOS it intentionally does not add
Linux packages; capture and graphics use Apple system frameworks.

## Capture

```console
# Capture the primary display.
cargo run --manifest-path unix-warp/Cargo.toml -- --display

# Capture the first window matching a title.
cargo run --manifest-path unix-warp/Cargo.toml -- --window "Ableton" --cursor
```

Linux capture uses the active desktop's X11 or Wayland capture backend. macOS
requires Screen Recording permission for the terminal or application launching
`unix-warp`.

## Linux stream sender

On Linux, `unix-warp` captures a local source, GPU-warps it, encodes H.264 with
FFmpeg's VAAPI encoder, and sends MPEG-TS over UDP. It also exposes the
protocol-compatible pointer-control server used by Warp clients. The default
`h264_vaapi` encoder targets the Intel/AMD VAAPI render device on this laptop:

```console
nix develop .#unix-warp
cargo run --manifest-path unix-warp/Cargo.toml -- \
  --display \
  --stream udp://RECEIVER_HOST:5004 \
  --control-bind 0.0.0.0:47053
```

For a window source, give `--input-origin X,Y` so remote pointer events map to
its desktop position:

```console
cargo run --manifest-path unix-warp/Cargo.toml -- \
  --window "Ableton" --input-origin 120,80 \
  --stream udp://RECEIVER_HOST:5004 --control-bind 0.0.0.0:47053
```

`--stream-size`, `--bitrate`, `--gop`, `--multicast-ttl`, `--encoder`, and
`--vaapi-device` configure the sender. Use `--encoder h264_nvenc` on compatible
NVIDIA hardware. The warp and VAAPI H.264 encode run on the GPU; the current
wgpu-to-FFmpeg integration reads the rendered frame back through system memory
between those stages.

## Pointer transfer

Input transfer is opt-in because it injects real native pointer events. The
preview's mouse position is inverse-warped into the captured source, then sent
to the operating system:

```console
# A primary-display capture starts at desktop coordinate 0,0.
cargo run --manifest-path unix-warp/Cargo.toml -- --display --transfer-input

# Window captures need the window's desktop-space top-left position.
cargo run --manifest-path unix-warp/Cargo.toml -- \
  --window "Ableton" --transfer-input --input-origin 120,80
```

macOS requires Accessibility permission in addition to Screen Recording.
Linux pointer injection uses Enigo's X11 backend by default. A Wayland
compositor may reject synthetic input unless it exposes a compatible input
emulation protocol; this is an intentional security boundary and is reported
as an initialization or transfer error rather than silently ignored.

## Windows stream receiver

The receiver decodes the Windows sender's MPEG-TS/H.264 stream through `ffmpeg`,
renders it in the same GPU-warped preview, and sends preview pointer events to the
sender's `--control-bind` TCP listener. Start the receiver before the sender so it
binds the UDP port first:

```console
nix develop .#unix-warp
cargo run --manifest-path unix-warp/Cargo.toml -- \
  --receive udp://0.0.0.0:5004 \
  --receive-size 1080x1080 \
  --control WINDOWS_HOST:47053
```

Then start the Windows sender with:

```console
window-warp --display --stream udp://LINUX_HOST:5004 --control-bind 0.0.0.0:47053
```

`--control` forwards pointer motion and left, right, or middle button events to
the sender. It controls the remote desktop, so use a safe preview and close it
with `Esc` when finished.

## Controls

`--inner-radius`, `--outer-radius`, `--start-angle`, `--angle-span`,
`--counter-clockwise`, and `--supersample` configure the warp. `--overlay`
creates a borderless always-on-top preview.

While the preview is open, `Left`/`Right` rotates it, `Up`/`Down` changes the
inner radius, `PageUp`/`PageDown` changes supersampling, `Space` freezes the
last capture frame and stream frame, and `Esc` closes the app.
