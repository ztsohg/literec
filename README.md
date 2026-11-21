# LiteRec

Lightweight screen/window recorder for old Windows PCs.
Rust + egui, DXGI / Windows Graphics Capture, WASAPI audio, x264 via ffmpeg.

## Manual Build
1. Install Rust (https://rustup.rs), MSVC toolchain.
2. `cargo build --release`  ->  `target\release\literec.exe`
   (If the `wasapi` crate fails to compile: `cargo build --release --no-default-features` disables per-app audio.)
3. Put a recent `ffmpeg.exe` (6.x or newer; "release essentials" from gyan.dev) next to `literec.exe` or on PATH.

## Features
- Display capture (no yellow border via DXGI Desktop Duplication) or window capture (Graphics Capture)
- OBS-style dock UI (preview, Sources, Audio Mixer, Controls, Settings dialog) with live preview
- Audio: pick desktop output device, microphone, and/or a single application; gain %, mute, live meters
- Presets (Potato / Balanced / Smooth / Quality)
- Tray mode, live CPU % and dropped-frame counter
- Each recording writes `literec_<time>.log` (ffmpeg messages) beside the video

## Notes
- Per-app audio needs Windows 10 build 19041 (2004) or newer.
- DXGI can't capture a single window and can't capture some protected content; use Window mode / uncheck DXGI for that.
- Display numbering in DXGI mode follows your GPU's outputs and may differ from Windows' display order.
- Settings are saved to %APPDATA%\LiteRec\settings.ini

## Layout
- `capture.rs` DXGI + Graphics Capture sessions   - `audio.rs` always-on audio engine
- `recorder.rs` FPS pacer + ffmpeg pipeline        - `sys.rs` app list + CPU meter
- `main.rs` UI, presets, settings, tray
