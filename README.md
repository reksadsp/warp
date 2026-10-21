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
