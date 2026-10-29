//! Accept Explorer file drops on the dock; only the Recycle Bin ("trash") tile consumes them.
use std::path::PathBuf;

use windows::Win32::Foundation::{BOOL, HWND, POINT};
use windows::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, DragQueryPoint, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    ChangeWindowMessageFilterEx, CHANGEFILTERSTRUCT, MSGFLT_ALLOW, WM_DROPFILES,
};

/// `WM_COPYGLOBALDATA` carries the HDROP across integrity levels with `WM_DROPFILES`.
const WM_COPYGLOBALDATA: u32 = 0x0049;

/// Allows `WM_DROPFILES` from Explorer onto `hwnd`.
pub fn enable(hwnd: HWND) {
    // SAFETY: hwnd is our dock; MSGFLT_ALLOW lets medium-IL Explorer post drop messages.
    unsafe {
        let mut info = CHANGEFILTERSTRUCT {
            cbSize: std::mem::size_of::<CHANGEFILTERSTRUCT>() as u32,
            ..Default::default()
        };
        let _ = ChangeWindowMessageFilterEx(hwnd, WM_DROPFILES, MSGFLT_ALLOW, Some(&mut info as *mut _));
        let _ = ChangeWindowMessageFilterEx(hwnd, WM_COPYGLOBALDATA, MSGFLT_ALLOW, Some(&mut info as *mut _));
        DragAcceptFiles(hwnd, BOOL(1));
    }
}

/// True when `msg` is the file-drop message.
pub fn is_drop_message(msg: u32) -> bool {
    msg == WM_DROPFILES
}

/// Paths in an HDROP, plus the drop point in the window's client pixels. Finishes the HDROP.
pub fn take_from_wparam(wparam: usize) -> (POINT, Vec<PathBuf>) {
    let hdrop = HDROP(wparam as _);
    let mut pt = POINT::default();
    // SAFETY: hdrop is valid for the duration of WM_DROPFILES handling.
    unsafe {
        let _ = DragQueryPoint(hdrop, &mut pt);
    }
    let count = unsafe { DragQueryFileW(hdrop, 0xFFFF_FFFF, None) };
    let mut paths = Vec::with_capacity(count as usize);
    for i in 0..count {
        let needed = unsafe { DragQueryFileW(hdrop, i, None) };
        if needed == 0 {
            continue;
        }
        let mut buf = vec![0u16; needed as usize];
        let n = unsafe { DragQueryFileW(hdrop, i, Some(&mut buf)) };
        if n == 0 {
            continue;
        }
        let nul = buf.iter().position(|&c| c == 0).unwrap_or(n as usize);
        let path = String::from_utf16_lossy(&buf[..nul]);
        if !path.is_empty() {
            paths.push(PathBuf::from(path));
        }
    }
    unsafe {
        DragFinish(hdrop);
    }
    (pt, paths)
}
