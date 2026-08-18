# warp
Screen warping and capture modules.

- `src/` — cross platform capture and wgpu rendering.
- `window-warp/` — Windows only window capture warped from a rectangle onto a disk with
  Direct3D 11. Its own workspace, see [window-warp/README.md](window-warp/README.md).

## 🚀 Setup after installing Determinate Nix on Macos:
### 1. Switch the flake to nix-darwin
```bash
nix run nix-darwin -- switch --flake github:lnl7/nix-darwin
nix develop
```
