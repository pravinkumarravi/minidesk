# miniDesk 🖥️⚡

[![Rust](https://img.shields.io/badge/rust-2021_edition-orange.svg)](https://www.rust-lang.org)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![GUI: egui](https://img.shields.io/badge/GUI-egui_0.29-lightblue.svg)](https://github.com/emilk/egui)

**miniDesk** is a production-grade, modular, secure, high-performance remote desktop application written in Rust. It offers low-latency screen capture and streaming, stuck-key-safe remote input injection, bounded clipboard synchronization, SHA-256 chunked file transfer, and self-hosted control plane signaling.

---

## 🌟 Key Features

- **⚡ High-Performance Screen Capture & Streaming**:
  - Modular codecs with **LZ4** frame compression.
  - Native Windows capture (Win32 GDI with session isolation recovery & DXGI hook points).
  - macOS platform adapter (**ScreenCaptureKit** / **CGDisplayStream**).
  - Linux platform adapter (**X11 XShm** & **Wayland PipeWire portal**).
  - Dynamic multi-monitor discovery and runtime display switching.
- **🛡️ Mutual Ed25519 Cryptographic Verification**:
  - Hardware-stable device identification (`DeviceId`).
  - Zero plain-text secrets; mutual Ed25519 challenge-response peer authentication on every session.
  - Granular capability-based access control (`VIEW_SCREEN`, `INPUT_CONTROL`, `CLIPBOARD_READ`, `CLIPBOARD_WRITE`, `FILE_TRANSFER`, `AUDIO_STREAM`).
- **⌨️ Stuck-Key Safe Input Injection**:
  - Native platform input dispatchers with monotonic sequence tracking.
  - Automatic key/mouse release guards that neutralize orphaned modifiers (`Ctrl`, `Alt`, `Shift`, `Meta`) on disconnect or session reset.
- **📋 Bounded Bi-Directional Clipboard Sync**:
  - Memory-bounded (1 MB) UTF-8 text synchronization.
  - Capability gating with SHA-256 hash tracking for loop/echo suppression.
- **📁 Robust Chunked File Transfer**:
  - Bounded 64 KB chunk streaming with memory pressure caps.
  - Strict path sanitization against directory traversal (`../`, absolute paths, Windows reserved DOS device names `CON`, `PRN`, `AUX`, `NUL`, `COM1-9`, `LPT1-9`).
  - Safe `.part` temporary download isolation and streaming SHA-256 verification before atomic finalize.
- **🌐 Dual Connection Modes**:
  - **Direct IP Entry**: Connect peer-to-peer over local LAN or direct VPN without external servers.
  - **Self-Hosted Signaling Server (`minidesk-server`)**: Ephemeral WebSocket signaling router and TURN credential token broker for NAT traversal.
- **🤖 Headless Unattended Access Daemon (`minidesk-agent`)**:
  - Secure service agent for server administration and unattended workstations.
  - Disabled by default; requires explicit enrollment of authorized peer Ed25519 public keys with full audit logging.

---

## 🏗️ Workspace Architecture

The miniDesk codebase is organized into **18 independently testable Rust crates**:

```text
miniDesk/
├── apps/
│   ├── minidesk-desktop/          # Desktop GUI client & host application (egui/eframe)
│   ├── minidesk-server/           # Self-hosted signaling & TURN broker service (Axum)
│   └── minidesk-agent/            # Headless unattended background service daemon
├── crates/
│   ├── minidesk-domain/           # Core domain models, SessionId, DeviceId, capabilities
│   ├── minidesk-protocol/         # Framed binary protocol messages & codecs
│   ├── minidesk-security/         # Ed25519 identity, signature generation & verification
│   ├── minidesk-transport/        # Direct TCP transport & authenticated handshake
│   ├── minidesk-signaling-client/ # WebSocket signaling client & TURN discovery
│   ├── minidesk-codecs/           # LZ4 frame encoders/decoders
│   ├── minidesk-clipboard/        # Bounded clipboard sync & echo suppression
│   ├── minidesk-file-transfer/    # Bounded 64KB chunked file streaming & sanitization
│   ├── minidesk-storage/          # SQLite persistence for sessions & trusted profiles
│   ├── minidesk-config/           # Type-safe configuration with serde & policy validation
│   ├── minidesk-logging/          # Structured logging with credential & clipboard redaction
│   ├── minidesk-platform-api/     # Platform traits (Capturer, Injector, Audio, Status)
│   ├── minidesk-platform-windows/ # Windows Win32 GDI capture, SendInput, multi-monitor
│   ├── minidesk-platform-macos/   # macOS platform adapter (ScreenCaptureKit, Quartz)
│   ├── minidesk-platform-linux/   # Linux platform adapter (X11 XShm, Wayland PipeWire)
│   ├── minidesk-app-core/         # Cross-platform application state machine & controller
│   └── minidesk-ui-egui/          # Pure immediate-mode egui/eframe UI & viewport
└── docs/
    └── architecture/
        └── ARCHITECTURE_SPECIFICATION.md
```

---

## 🚀 Getting Started

### Prerequisites
- [Rust](https://www.rust-lang.org/tools/install) (1.75+ recommended)
- `cargo` package manager

### Building from Source

```bash
# Clone the repository
git clone https://github.com/pravinkumarravi/minidesk.git
cd minidesk

# Run test suite across all crates
cargo test --workspace

# Build optimized release binaries
cargo build --release
```

Release binaries will be generated in `target/release/`:
- `minidesk-desktop.exe` (or `minidesk-desktop` on macOS/Linux)
- `minidesk-server.exe`
- `minidesk-agent.exe`

---

## 📖 Usage Guide

### 1. Running the Desktop Application

```bash
./target/release/minidesk-desktop
```

- **Host Mode**: Click **"Start Hosting"** to bind to port 33890 (or configured port). Share your Device ID or IP address with the remote operator.
- **Client Mode**: Enter the host's IP and port (e.g. `192.168.1.100:33890`), verify the peer's Ed25519 identity badge, and click **"Connect"**.
- **Interactive Controls**:
  - Live viewport with aspect-ratio-correct scaling.
  - Monitor switcher dropdown to toggle between connected displays.
  - File Transfer panel with incoming file approvals and real-time progress bars.
  - Reset Inputs button to instantly neutralize keys and mouse buttons.

### 2. Self-Hosted Signaling Server

Run the lightweight signaling and control plane server:

```bash
./target/release/minidesk-server --listen 0.0.0.0:8080
```

- REST Health Endpoint: `GET /health`
- WebSocket Signaling Hub: `ws://<host>:8080/ws/signal/<device_id>`
- Ephemeral TURN Token Broker: `POST /api/v1/turn/credentials`

### 3. Unattended Service Daemon

To install and run miniDesk as a background workstation daemon:

```bash
# Enroll an authorized operator's public key (hex)
./target/release/minidesk-agent --enroll-key <64-char-hex-ed25519-public-key>

# Start the agent service
./target/release/minidesk-agent --config agent-config.json
```

---

## 🔒 Security Architecture

1. **Mutual Cryptographic Identity**: Every device generates a dedicated Ed25519 keypair saved with restricted filesystem permissions. No plain-text passwords or shared secrets.
2. **Replay & Injection Protection**: Protocol frames are strictly sized (max payload bounded to 16 MB) with length prefixes, type validation, and monotonic sequence numbers.
3. **Data Protection & Redaction**: The structured logging system automatically redacts clipboard contents, IP addresses, and private key representations.
4. **Traversal Defense**: File transfers strictly sanitize destination file paths, eliminating absolute path hijacking and directory climbing (`../`).

---

## 📄 License

This project is licensed under the [MIT License](LICENSE).
