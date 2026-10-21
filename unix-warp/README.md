
# unix-warp

`unix-warp` captures a window or display on Linux or macOS, maps the captured
rectangle around a disk with `wgpu`, and can translate mouse movement and
buttons on that disk back to their source coordinates.

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

## Controls

`--inner-radius`, `--outer-radius`, `--start-angle`, `--angle-span`,
`--counter-clockwise`, and `--supersample` configure the warp. `--overlay`
creates a borderless always-on-top preview.
