# Mirr

A Windows 11 bridge for Apple's wireless sharing ecosystem, wrapped in a visionOS-style **Liquid Glass** UI.

**Features:** AirPlay receive (screen + audio, docked right inside the app) · AirPlay 2 audio · AirCast (send PC audio to AirPlay speakers) · LocalSend (cross-platform file transfer).

**Stack:** Tauri 2 — Rust core + WebView2 UI. Discovery (mDNS) and LocalSend are implemented natively in Rust; the AirPlay engine (UxPlay/GStreamer) runs as a supervised sidecar process with a hidden console, and its video output is reparented straight into the main window.

---

## Highlights

- **In-app mirroring** — the mirror video no longer opens a separate "Direct3D11 renderer" window. A Win32 watcher finds the engine's video window, strips its decorations and docks it into the glass stage (`src-tauri/src/embed.rs`), rounded corners included. A Theater mode melts the chrome away.
- **No terminal windows** — the engine is spawned with `CREATE_NO_WINDOW`; its stdout is parsed for lifecycle events (`ready` / `connecting` / `stopped`) and surfaced in the UI instead.
- **Zero-setup start** — the receiver auto-starts with the app. Launch `Mirr.exe`, pick **Mirr** in the iOS Screen Mirroring sheet, done. No launcher scripts needed.
- **Liquid Glass UI** — frameless window with acrylic/mica, custom window controls, aurora backdrop, specular glass surfaces. Backdrop blur is limited to the three large surfaces; inner cards are lightweight translucent tiles, and ambient animation pauses while mirroring or hidden.

## Requirements

1. **Windows 11** (WebView2 preinstalled)
2. **UxPlay** — `winget install --id leapbtw.uxplay -e`
   (Mirr finds it in `C:\Program Files (x86)\uxplay-windows` automatically; a bundled `src-tauri/engines/uxplay.exe` takes precedence if present.)
3. First run: allow uxplay through the Windows firewall (TCP + UDP, private networks), and keep mDNS enabled.

## Run the UI in a browser (no toolchain)

The frontend has zero build step:

```powershell
cd mirr
python -m http.server 8777 --bind 127.0.0.1 --directory src
# open http://127.0.0.1:8777/index.html  (demo mode, mock devices)
```

## Build the native app

Prerequisites (one-time): [Rust](https://rustup.rs), MSVC C++ Build Tools, Node.

```powershell
cd mirr
npm install         # gets @tauri-apps/cli
npm run dev         # tauri dev — compiles Rust, launches the glass window
npm run build       # produces an .msi / .exe installer
# or, plain release binary:
cd src-tauri; cargo build --release   # -> target/release/mirr.exe
```

`scripts/setup-windows.ps1` automates the toolchain install + first run.

## How the mirror docking works

1. `engines.rs` spawns `uxplay.exe -n Mirr -nh -vsync no -fps 60` with a hidden console and pipes its output.
2. When a device starts mirroring, GStreamer creates its video window; the watcher thread in `embed.rs` finds any visible window owned by the engine PID.
3. It converts it to a `WS_CHILD` of the main window (`SetParent`), and keeps it glued to the rectangle the frontend reports for the video stage (`set_video_rect`, physical pixels), with a rounded GDI region to match the glass corners.
4. When the client disconnects the window dies; the watcher emits `mirror://ended` and the UI returns to the radar idle state.

## LocalSend

`src-tauri/src/localsend.rs` implements the **LocalSend v2** protocol natively:

- **Discovery:** UDP multicast `224.0.0.167:53317`, announcing every 5 s.
- **Receive:** HTTP server on `:53317` (`/info`, `/register`, `/prepare-upload`, `/upload`); files land in `%USERPROFILE%\Downloads\Mirr`.
- **Send:** pick a file (or drop one on the window) and it runs prepare-upload → upload.

> This build speaks plain HTTP. For full parity with the mobile apps (HTTPS by default), swap the `http_post` helper for `reqwest` with a permissive TLS verifier.

## Project layout

```
mirr/
  src/                  Liquid Glass frontend (static, no build step)
    index.html
    styles.css
    app.js              nav, engine state machine, video-rect reporting, Tauri-or-demo bridge
  src-tauri/
    Cargo.toml
    tauri.conf.json     frameless window + acrylic/mica, bundle config
    capabilities/       Tauri 2 permission set
    icons/
    src/
      main.rs           Tauri entry, command registry, receiver auto-start
      embed.rs          Win32 video-window docking (SetParent + rounded region)
      engines.rs        sidecar supervisor — hidden console, log parsing, lifecycle events
      discovery.rs      mDNS browse for AirPlay screen + RAOP audio targets
      localsend.rs      LocalSend v2 (multicast + HTTP)
  scripts/setup-windows.ps1
  Start Mirr.bat        convenience launcher for the release build
```

## License notes

Mirr's own code is MIT. UxPlay/GStreamer are GPL and run as separate, unmodified processes (sidecar boundary).
