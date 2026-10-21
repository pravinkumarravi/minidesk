# miniDesk v0.1.0 — Initial Production Release 🖥️⚡

Welcome to the initial official release of **miniDesk**, a high-performance, modular, secure cross-platform remote desktop system written entirely in Rust.

---

### 📦 Release Assets

- **`minidesk-v0.1.0-windows-x86_64.zip`**: Complete Windows 64-bit distribution package containing:
  - `minidesk-desktop.exe` (12.78 MB) — Immediate-mode GUI client & host application (`egui` / `eframe`)
  - `minidesk-server.exe` (2.65 MB) — Self-hosted WebSocket signaling router & TURN broker (`Axum`)
  - `minidesk-agent.exe` (3.85 MB) — Headless background unattended access daemon
  - `config.example.json` — Comprehensive configuration template
  - `README.md` & `LICENSE` (MIT)
- **`SHA256SUMS.txt`**: SHA-256 integrity checksum manifest

---

### 🚀 Highlights & Capabilities

- **⚡ Screen Streaming & Display Management**:
  - Low-latency **LZ4** compression codecs with modular video pipeline.
  - Windows screen capture (Win32 GDI with session isolation fallback & DXGI hook points).
  - macOS platform adapter (**ScreenCaptureKit** / **CGDisplayStream**).
  - Linux platform adapter (**X11 XShm** & **Wayland PipeWire portal**).
  - Dynamic multi-monitor discovery and runtime display switching.
- **🛡️ Cryptographic Security**:
  - Mutual **Ed25519** challenge-response peer authentication on every session.
  - Hardware-stable device identity (`DeviceId`).
  - Strict capability-based access control (`VIEW_SCREEN`, `INPUT_CONTROL`, `CLIPBOARD_READ`, `CLIPBOARD_WRITE`, `FILE_TRANSFER`, `AUDIO_STREAM`).
- **⌨️ Stuck-Key Safe Input Injection**:
  - Monotonic sequence tracking and automatic release guards neutralizing orphaned modifier keys (`Ctrl`, `Alt`, `Shift`, `Meta`) upon disconnect or reset.
- **📋 Bounded Clipboard Sync**:
  - 1 MB bounded UTF-8 text sync with SHA-256 echo-loop suppression.
- **📁 Robust Chunked File Transfer**:
  - Bounded 64 KB chunk streaming with memory pressure caps.
  - Strict path traversal defense (`../`, absolute paths, Windows reserved DOS device names).
  - Isolated `.part` temporary downloads with streaming SHA-256 hash verification.
- **🤖 Headless Unattended Access Daemon**:
  - Dedicated background service agent for server management and remote administration.
  - Strictly disabled by default; requires explicit enrollment of authorized peer Ed25519 public keys.
- **🌐 Dual Connectivity**:
  - Direct IP / LAN peer-to-peer connection.
  - Self-hosted signaling router (`minidesk-server`) for NAT traversal and ephemeral session coordination.
