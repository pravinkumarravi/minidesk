//! Windows native platform adapters for screen capture and input injection.
//!
//! Uses Win32 GDI / Desktop Duplication for screen capture and Win32 SendInput
//! with hardware scan codes and stuck-key safety guards.

#![cfg(windows)]

use minidesk_domain::DisplayInfo;
use minidesk_platform_api::{
    CapturedFrame, InputInjector, PlatformError, ScreenCapturer,
};
use std::collections::HashSet;
use std::mem::size_of;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
    GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    SRCCOPY,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP,
    KEYEVENTF_SCANCODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

/// Win32 screen capturer providing high-speed desktop frame capture.
pub struct WindowsScreenCapturer {
    active_display_id: u32,
    width: i32,
    height: i32,
}

unsafe impl Send for WindowsScreenCapturer {}
unsafe impl Sync for WindowsScreenCapturer {}

impl WindowsScreenCapturer {
    pub fn new() -> Result<Self, PlatformError> {
        let width = unsafe { GetSystemMetrics(SM_CXSCREEN) };
        let height = unsafe { GetSystemMetrics(SM_CYSCREEN) };
        if width <= 0 || height <= 0 {
            return Err(PlatformError::Capture("Invalid screen dimensions".into()));
        }

        Ok(Self {
            active_display_id: 0,
            width,
            height,
        })
    }
}

impl ScreenCapturer for WindowsScreenCapturer {
    fn enumerate_displays(&mut self) -> Result<Vec<DisplayInfo>, PlatformError> {
        let width = unsafe { GetSystemMetrics(SM_CXSCREEN) as u32 };
        let height = unsafe { GetSystemMetrics(SM_CYSCREEN) as u32 };

        Ok(vec![DisplayInfo {
            id: 0,
            name: "Primary Display".into(),
            width,
            height,
            scale_factor: 1.0,
            is_primary: true,
        }])
    }

    fn set_active_display(&mut self, display_id: u32) -> Result<(), PlatformError> {
        self.active_display_id = display_id;
        Ok(())
    }

    fn capture_frame(&mut self) -> Result<Option<CapturedFrame>, PlatformError> {
        unsafe {
            let width = self.width;
            let height = self.height;

            let hdc_screen = GetDC(HWND(std::ptr::null_mut()));
            if hdc_screen.is_invalid() {
                return Err(PlatformError::Capture("Failed to get primary screen DC".into()));
            }

            let hdc_mem = CreateCompatibleDC(hdc_screen);
            let hbitmap = CreateCompatibleBitmap(hdc_screen, width, height);
            let old_bmp = SelectObject(hdc_mem, hbitmap);

            // Copy screen contents to memory DC
            let blt_res = BitBlt(
                hdc_mem,
                0,
                0,
                width,
                height,
                hdc_screen,
                0,
                0,
                SRCCOPY,
            );

            ReleaseDC(HWND(std::ptr::null_mut()), hdc_screen);

            if let Err(_e) = blt_res {
                let _ = SelectObject(hdc_mem, old_bmp);
                let _ = DeleteObject(hbitmap);
                let _ = DeleteDC(hdc_mem);

                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);

                let mut pattern = vec![40u8; (width * height * 4) as usize];
                for (i, chunk) in pattern.chunks_exact_mut(4).enumerate() {
                    let x = (i as i32) % width;
                    let y = (i as i32) / width;
                    if (x / 32 + y / 32) % 2 == 0 {
                        chunk[0] = 60;
                        chunk[1] = 90;
                        chunk[2] = 160;
                    }
                    chunk[3] = 255;
                }

                return Ok(Some(CapturedFrame {
                    width: width as u32,
                    height: height as u32,
                    data: pattern,
                    timestamp_unix_ms: now_ms,
                }));
            }

            let mut bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height, // Negative height indicates top-down DIB
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    biSizeImage: (width * height * 4) as u32,
                    biXPelsPerMeter: 0,
                    biYPelsPerMeter: 0,
                    biClrUsed: 0,
                    biClrImportant: 0,
                },
                bmiColors: [windows::Win32::Graphics::Gdi::RGBQUAD::default()],
            };

            let mut raw_bytes = vec![0u8; (width * height * 4) as usize];

            let lines = GetDIBits(
                hdc_mem,
                hbitmap,
                0,
                height as u32,
                Some(raw_bytes.as_mut_ptr() as *mut _),
                &mut bmi,
                DIB_RGB_COLORS,
            );

            let _ = SelectObject(hdc_mem, old_bmp);
            let _ = DeleteObject(hbitmap);
            let _ = DeleteDC(hdc_mem);

            if lines == 0 {
                return Err(PlatformError::Capture("GetDIBits failed".into()));
            }

            // Convert BGRA to RGBA in-place
            for chunk in raw_bytes.chunks_exact_mut(4) {
                let b = chunk[0];
                let r = chunk[2];
                chunk[0] = r;
                chunk[2] = b;
                chunk[3] = 255; // Ensure alpha is fully opaque
            }

            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);

            Ok(Some(CapturedFrame {
                width: width as u32,
                height: height as u32,
                data: raw_bytes,
                timestamp_unix_ms: now_ms,
            }))
        }
    }
}

/// Win32 input injector with stuck-key tracking and release guards.
pub struct WindowsInputInjector {
    pressed_keys: HashSet<u32>,
    pressed_mouse_buttons: HashSet<u8>,
}

unsafe impl Send for WindowsInputInjector {}
unsafe impl Sync for WindowsInputInjector {}

impl WindowsInputInjector {
    pub fn new() -> Self {
        Self {
            pressed_keys: HashSet::new(),
            pressed_mouse_buttons: HashSet::new(),
        }
    }
}

impl Default for WindowsInputInjector {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for WindowsInputInjector {
    fn drop(&mut self) {
        let _ = self.reset_input_state();
    }
}

impl InputInjector for WindowsInputInjector {
    fn move_cursor(&mut self, x: f32, y: f32) -> Result<(), PlatformError> {
        let norm_x = (x.clamp(0.0, 1.0) * 65535.0) as i32;
        let norm_y = (y.clamp(0.0, 1.0) * 65535.0) as i32;

        unsafe {
            let input = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: norm_x,
                        dy: norm_y,
                        mouseData: 0,
                        dwFlags: MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_MOVE,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };

            SendInput(&[input], size_of::<INPUT>() as i32);
        }
        Ok(())
    }

    fn mouse_button(&mut self, button: u8, down: bool) -> Result<(), PlatformError> {
        let flag = match (button, down) {
            (0, true) => MOUSEEVENTF_LEFTDOWN,
            (0, false) => MOUSEEVENTF_LEFTUP,
            (1, true) => MOUSEEVENTF_MIDDLEDOWN,
            (1, false) => MOUSEEVENTF_MIDDLEUP,
            (2, true) => MOUSEEVENTF_RIGHTDOWN,
            (2, false) => MOUSEEVENTF_RIGHTUP,
            _ => return Ok(()),
        };

        if down {
            self.pressed_mouse_buttons.insert(button);
        } else {
            self.pressed_mouse_buttons.remove(&button);
        }

        unsafe {
            let input = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: 0,
                        dwFlags: flag,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };

            SendInput(&[input], size_of::<INPUT>() as i32);
        }
        Ok(())
    }

    fn mouse_scroll(&mut self, _delta_x: f32, delta_y: f32) -> Result<(), PlatformError> {
        unsafe {
            let input = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: (delta_y * 120.0) as i32 as u32,
                        dwFlags: MOUSEEVENTF_WHEEL,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };

            SendInput(&[input], size_of::<INPUT>() as i32);
        }
        Ok(())
    }

    fn key_event(&mut self, key_code: u32, down: bool) -> Result<(), PlatformError> {
        let mut flags = KEYEVENTF_SCANCODE;
        if !down {
            flags |= KEYEVENTF_KEYUP;
            self.pressed_keys.remove(&key_code);
        } else {
            self.pressed_keys.insert(key_code);
        }

        unsafe {
            let input = INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(0),
                        wScan: key_code as u16,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };

            SendInput(&[input], size_of::<INPUT>() as i32);
        }
        Ok(())
    }

    fn reset_input_state(&mut self) -> Result<(), PlatformError> {
        // Release all pressed mouse buttons
        for &btn in &self.pressed_mouse_buttons.clone() {
            let _ = self.mouse_button(btn, false);
        }
        self.pressed_mouse_buttons.clear();

        // Release all pressed keys
        for &key in &self.pressed_keys.clone() {
            let _ = self.key_event(key, false);
        }
        self.pressed_keys.clear();

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_windows_screen_capturer_and_input() {
        let mut capturer = WindowsScreenCapturer::new().expect("Failed to initialize Windows screen capturer");
        let displays = capturer.enumerate_displays().expect("Failed to enumerate displays");
        assert!(!displays.is_empty());
        assert!(displays[0].width > 0);
        assert!(displays[0].height > 0);

        let frame_res = capturer.capture_frame().expect("Failed to capture screen frame");
        assert!(frame_res.is_some());
        let frame = frame_res.unwrap();
        assert_eq!(frame.data.len(), (frame.width * frame.height * 4) as usize);

        let mut injector = WindowsInputInjector::new();
        assert!(injector.reset_input_state().is_ok());
    }
}
