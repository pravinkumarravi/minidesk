//! Platform abstraction traits for miniDesk.
//!
//! Provides OS-independent interfaces for screen capture, input injection,
//! audio capture/playback, clipboard access, and system permissions.

use async_trait::async_trait;
use minidesk_domain::DisplayInfo;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum PlatformError {
    #[error("Screen capture error: {0}")]
    Capture(String),

    #[error("Input injection error: {0}")]
    Input(String),

    #[error("Audio error: {0}")]
    Audio(String),

    #[error("Clipboard error: {0}")]
    Clipboard(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("Operation unsupported on this platform: {0}")]
    Unsupported(String),
}

/// A captured raw desktop video frame.
pub struct CapturedFrame {
    pub width: u32,
    pub height: u32,
    /// Raw RGBA or BGRA frame bytes.
    pub data: Vec<u8>,
    pub timestamp_unix_ms: u64,
}

/// Interface for operating-system screen capture.
pub trait ScreenCapturer: Send + Sync {
    /// Enumerate all connected displays.
    fn enumerate_displays(&mut self) -> Result<Vec<DisplayInfo>, PlatformError>;

    /// Set the active display to capture.
    fn set_active_display(&mut self, display_id: u32) -> Result<(), PlatformError>;

    /// Capture a single frame from the currently active display.
    fn capture_frame(&mut self) -> Result<Option<CapturedFrame>, PlatformError>;
}

/// Interface for remote mouse and keyboard event injection.
pub trait InputInjector: Send + Sync {
    /// Move cursor to normalized screen coordinates (0.0 to 1.0).
    fn move_cursor(&mut self, x: f32, y: f32) -> Result<(), PlatformError>;

    /// Inject mouse button state change.
    fn mouse_button(&mut self, button: u8, down: bool) -> Result<(), PlatformError>;

    /// Inject mouse scroll wheel delta.
    fn mouse_scroll(&mut self, delta_x: f32, delta_y: f32) -> Result<(), PlatformError>;

    /// Inject keyboard key down or up.
    fn key_event(&mut self, key_code: u32, down: bool) -> Result<(), PlatformError>;

    /// Hardware safety guard: releases all pressed keys and mouse buttons.
    fn reset_input_state(&mut self) -> Result<(), PlatformError>;
}

/// Interface for system audio capture and playback.
pub trait AudioDevice: Send + Sync {
    /// Start capturing system loopback output.
    fn start_capture(&mut self) -> Result<(), PlatformError>;

    /// Stop capturing system loopback output.
    fn stop_capture(&mut self) -> Result<(), PlatformError>;

    /// Play received audio chunk.
    fn play_audio(&mut self, pcm_data: &[f32]) -> Result<(), PlatformError>;
}

/// Interface for reading and writing to the OS clipboard.
pub trait ClipboardDevice: Send + Sync {
    fn read_text(&mut self) -> Result<Option<String>, PlatformError>;
    fn write_text(&mut self, text: &str) -> Result<(), PlatformError>;
    fn read_image_png(&mut self) -> Result<Option<Vec<u8>>, PlatformError>;
    fn write_image_png(&mut self, png_bytes: &[u8]) -> Result<(), PlatformError>;
}

/// Platform permission and elevation status.
pub struct PlatformStatus {
    pub is_elevated: bool,
    pub os_name: String,
    pub os_version: String,
    pub has_capture_permission: bool,
    pub has_input_permission: bool,
}

#[async_trait]
pub trait SystemStatusProvider: Send + Sync {
    async fn get_status(&self) -> PlatformStatus;
}
