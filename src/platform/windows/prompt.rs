//! A small native text-input dialog ("Keep awake for: [ 45m ]  OK  Cancel"),
//! run on its own thread with its own message loop so the tray stays responsive.

use std::ptr::{null, null_mut};
use std::thread;

use log::error;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{COLOR_BTNFACE, CreateFontIndirectW, DeleteObject, HFONT};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::EM_SETSEL;
use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CREATESTRUCTW, CreateWindowExW,
    DefWindowProcW, DestroyWindow, DispatchMessageW, ES_AUTOHSCROLL, GWLP_USERDATA, GetMessageW,
    GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, HMENU, IDC_ARROW, IDCANCEL, IDOK,
    IsDialogMessageW, LoadCursorW, MSG, NONCLIENTMETRICSW, PostQuitMessage, RegisterClassW,
    SPI_GETNONCLIENTMETRICS, SPI_GETWORKAREA, SW_SHOW, SendMessageW, SetForegroundWindow,
    SetWindowLongPtrW, ShowWindow, SystemParametersInfoW, TranslateMessage, WM_CLOSE, WM_COMMAND,
    WM_DESTROY, WM_NCCREATE, WM_SETFONT, WNDCLASSW, WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE,
    WS_EX_DLGMODALFRAME, WS_EX_TOPMOST, WS_POPUP, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
};

use super::wide;

struct State {
    edit: HWND,
    result: Option<String>,
}

/// Show the dialog; `done(value, true)` is called with the entered text, or
/// `None` if cancelled.
pub fn prompt_duration(
    title: &str,
    message: &str,
    initial: &str,
    done: Box<dyn FnOnce(Option<String>, bool) + Send>,
) {
    let (title, message, initial) = (title.to_owned(), message.to_owned(), initial.to_owned());
    thread::spawn(move || {
        // SAFETY: all windows are created, used and destroyed on this thread.
        let result = unsafe { run(&title, &message, &initial) };
        done(result, true);
    });
}

unsafe fn run(title: &str, message: &str, initial: &str) -> Option<String> {
    unsafe {
        let hinstance = GetModuleHandleW(null());
        let class = wide("PerkPromptDialog");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: (COLOR_BTNFACE + 1) as usize as _,
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc); // fails harmlessly if already registered

        let dpi = GetDpiForSystem().max(96) as i32;
        let px = |v: i32| v * dpi / 96;

        // Layout in device-independent pixels.
        let (cw, ch) = (px(360), px(150));
        let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
        let ex_style = WS_EX_DLGMODALFRAME | WS_EX_TOPMOST;
        let mut frame = RECT {
            left: 0,
            top: 0,
            right: cw,
            bottom: ch,
        };
        AdjustWindowRectEx(&mut frame, style, 0, ex_style);
        let (w, h) = (frame.right - frame.left, frame.bottom - frame.top);

        let mut work: RECT = std::mem::zeroed();
        SystemParametersInfoW(SPI_GETWORKAREA, 0, (&mut work as *mut RECT).cast(), 0);
        let x = work.left + ((work.right - work.left) - w) / 2;
        let y = work.top + ((work.bottom - work.top) - h) / 2;

        // Owned through a raw pointer: the window procedure accesses it too.
        let state = Box::into_raw(Box::new(State {
            edit: null_mut(),
            result: None,
        }));
        let title_w = wide(title);
        let hwnd = CreateWindowExW(
            ex_style,
            class.as_ptr(),
            title_w.as_ptr(),
            style,
            x,
            y,
            w,
            h,
            null_mut(),
            null_mut(),
            hinstance,
            state.cast(),
        );
        if hwnd.is_null() {
            error!("could not create input dialog");
            drop(Box::from_raw(state));
            return None;
        }

        let font = message_font();
        let child = |class: &str, text: &str, style: u32, ex: u32, x, y, w, h, id: i32| {
            let (c, t) = (wide(class), wide(text));
            let hwnd_child = CreateWindowExW(
                ex,
                c.as_ptr(),
                t.as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                px(x),
                px(y),
                px(w),
                px(h),
                hwnd,
                id as usize as HMENU,
                hinstance,
                null(),
            );
            if !font.is_null() {
                SendMessageW(hwnd_child, WM_SETFONT, font as usize, 1);
            }
            hwnd_child
        };

        child("STATIC", message, 0, 0, 16, 14, 328, 44, -1);
        (*state).edit = child(
            "EDIT",
            initial,
            WS_TABSTOP | ES_AUTOHSCROLL as u32,
            WS_EX_CLIENTEDGE,
            16,
            62,
            328,
            26,
            100,
        );
        child(
            "BUTTON",
            "OK",
            WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
            0,
            172,
            106,
            82,
            28,
            IDOK,
        );
        child(
            "BUTTON",
            "Cancel",
            WS_TABSTOP | BS_PUSHBUTTON as u32,
            0,
            262,
            106,
            82,
            28,
            IDCANCEL,
        );

        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
        SetFocus((*state).edit);
        SendMessageW((*state).edit, EM_SETSEL, 0, -1);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            // Handles Tab, Enter (IDOK) and Esc (IDCANCEL).
            if IsDialogMessageW(hwnd, &msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if !font.is_null() {
            DeleteObject(font);
        }
        Box::from_raw(state).result
    }
}

unsafe fn message_font() -> HFONT {
    unsafe {
        let mut metrics: NONCLIENTMETRICSW = std::mem::zeroed();
        metrics.cbSize = size_of::<NONCLIENTMETRICSW>() as u32;
        if SystemParametersInfoW(
            SPI_GETNONCLIENTMETRICS,
            metrics.cbSize,
            (&mut metrics as *mut NONCLIENTMETRICSW).cast(),
            0,
        ) == 0
        {
            return null_mut();
        }
        CreateFontIndirectW(&metrics.lfMessageFont)
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            let cs = &*(lparam as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
        match msg {
            WM_COMMAND => {
                let id = (wparam & 0xFFFF) as i32;
                if id == IDOK && !state.is_null() {
                    let edit = (*state).edit;
                    let len = GetWindowTextLengthW(edit).max(0) as usize;
                    let mut buf = vec![0u16; len + 1];
                    let n =
                        GetWindowTextW(edit, buf.as_mut_ptr(), buf.len() as i32).max(0) as usize;
                    (*state).result = Some(String::from_utf16_lossy(&buf[..n]));
                    DestroyWindow(hwnd);
                } else if id == IDCANCEL {
                    DestroyWindow(hwnd);
                }
                0
            }
            WM_CLOSE => {
                DestroyWindow(hwnd);
                0
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
