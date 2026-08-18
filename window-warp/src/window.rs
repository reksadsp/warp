use std::sync::Mutex;

use anyhow::{Context, Result};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VK_DOWN, VK_ESCAPE, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RIGHT, VK_SPACE, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::cli::WarpParams;

/// State shared between the window procedure and the render loop. Only one
/// window is ever created, so a process wide slot is enough.
static STATE: Mutex<Option<WindowState>> = Mutex::new(None);

#[derive(Debug, Clone, Copy)]
pub struct WindowState {
    pub quit: bool,
    pub paused: bool,
    pub resized_to: Option<(u32, u32)>,
    pub params: WarpParams,
}

pub struct OutputWindow {
    pub hwnd: HWND,
}

impl OutputWindow {
    pub fn create(title: &str, size: u32, overlay: bool, params: WarpParams) -> Result<Self> {
        *STATE.lock().unwrap() = Some(WindowState {
            quit: false,
            paused: false,
            resized_to: None,
            params,
        });

        unsafe {
            // Ignore failures: the app still works, it just renders blurry on
            // scaled displays.
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

            let instance: HINSTANCE = GetModuleHandleW(None)?.into();
            let class_name = w!("window_warp_output");

            let class = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                hbrBackground: HBRUSH::default(),
                lpszClassName: class_name,
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 {
                return Err(windows::core::Error::from_thread())
                    .context("failed to register the window class");
            }

            let (style, ex_style) = if overlay {
                (WS_POPUP | WS_VISIBLE, WS_EX_TOPMOST | WS_EX_APPWINDOW)
            } else {
                (WS_OVERLAPPEDWINDOW | WS_VISIBLE, WS_EX_APPWINDOW)
            };

            let mut rect = RECT {
                left: 0,
                top: 0,
                right: size as i32,
                bottom: size as i32,
            };
            AdjustWindowRectEx(&mut rect, style, false, ex_style)?;

            let title = to_wide(title);
            let hwnd = CreateWindowExW(
                ex_style,
                class_name,
                PCWSTR(title.as_ptr()),
                style,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                rect.right - rect.left,
                rect.bottom - rect.top,
                None,
                None,
                Some(instance),
                None,
            )?;

            Ok(Self { hwnd })
        }
    }

    pub fn client_size(&self) -> (u32, u32) {
        let mut rect = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rect);
        }
        (
            (rect.right - rect.left).max(1) as u32,
            (rect.bottom - rect.top).max(1) as u32,
        )
    }

    /// Drains the message queue and returns the current shared state.
    pub fn pump(&self) -> WindowState {
        unsafe {
            let mut message = MSG::default();
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        STATE.lock().unwrap().expect("window state is initialised")
    }

    pub fn clear_resize(&self) {
        if let Some(state) = STATE.lock().unwrap().as_mut() {
            state.resized_to = None;
        }
    }
}

extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let mut guard = STATE.lock().unwrap();
    let Some(state) = guard.as_mut() else {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };

    match msg {
        WM_CLOSE | WM_DESTROY => {
            state.quit = true;
            if msg == WM_DESTROY {
                unsafe { PostQuitMessage(0) };
            }
            LRESULT(0)
        }
        WM_SIZE => {
            let width = (lparam.0 & 0xffff) as u32;
            let height = ((lparam.0 >> 16) & 0xffff) as u32;
            if width > 0 && height > 0 {
                state.resized_to = Some((width, height));
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            apply_key(state, wparam.0 as u16);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn apply_key(state: &mut WindowState, key: u16) {
    let params = &mut state.params;
    match key {
        k if k == VK_ESCAPE.0 => state.quit = true,
        k if k == VK_SPACE.0 => state.paused = !state.paused,
        k if k == VK_LEFT.0 => {
            params.start_angle_deg = (params.start_angle_deg - 2.0).rem_euclid(360.0)
        }
        k if k == VK_RIGHT.0 => {
            params.start_angle_deg = (params.start_angle_deg + 2.0).rem_euclid(360.0)
        }
        k if k == VK_UP.0 => params.inner_radius = (params.inner_radius + 0.01).min(0.95),
        k if k == VK_DOWN.0 => params.inner_radius = (params.inner_radius - 0.01).max(0.0),
        k if k == VK_PRIOR.0 => params.supersample = (params.supersample + 1).min(8),
        k if k == VK_NEXT.0 => params.supersample = params.supersample.saturating_sub(1).max(1),
        _ => {}
    }
}

fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}
