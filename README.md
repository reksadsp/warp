# warp
Screen warping and capture modules.

- `src/` — cross platform capture and wgpu rendering.
- `window-warp/` — Windows only window capture warped from a rectangle onto a disk with
  Direct3D 11. Its own workspace, see [window-warp/README.md](window-warp/README.md).
- `unix-warp/` — Linux/macOS window or display capture, GPU disk warp, and opt-in pointer
  transfer. Its own workspace, see [unix-warp/README.md](unix-warp/README.md).

## Nix development shells

```bash
nix develop                 # root Warp Link crate
nix develop .#unix-warp    # Linux/macOS capture and warp application
```

## Windows sender interoperability test

The ignored `window_warp_e2e` test is a Linux-side UDP receiver and pointer-control
client for a running Windows `window-warp` sender. It validates MPEG-TS video packets,
then sends a pointer move and primary-button press/release through the sender's
`--control-bind` endpoint.

Start the test before the sender so its UDP receiver is ready. `WARP_E2E_POINTER` is a
normalized `X,Y` coordinate (each value is `0` through `65535`) and is both moved to and
clicked, so choose a safe location:

```bash
WARP_STREAM_BIND=0.0.0.0:5004 \
WARP_CONTROL_ADDR=WINDOWS_HOST:47053 \
WARP_E2E_POINTER=32768,32768 \
WARP_E2E_ALLOW_POINTER_INJECTION=1 \
cargo test --test window_warp_e2e -- --ignored --nocapture
```

Then start the Windows sender with
`--stream udp://LINUX_HOST:5004 --control-bind 0.0.0.0:47053`. The test waits up to
30 seconds by default; set `WARP_E2E_TIMEOUT_SECS` to adjust it.
