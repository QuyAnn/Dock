//! Active keyboard layout of the foreground window, as a short label ("ENG", "VI").
use std::cell::Cell;

use windows::Win32::UI::Input::KeyboardAndMouse::{keybd_event, GetKeyboardLayout, KEYEVENTF_KEYUP, VK_LWIN, VK_SPACE};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

thread_local! {
    static LAST: Cell<u16> = const { Cell::new(0x09) };
}

/// Vietnamese input method state read from its tray tooltip (EVKey / UniKey style: the tip
/// says what a click does, "Kích để tắt bàn phím Tiếng Việt" = Vietnamese is on right now).
fn ime_icon() -> Option<super::traysvc::TrayIcon> {
    let (_, icons) = super::traysvc::icons();
    icons.into_iter().find(|i| {
        let tip = i.tip.to_lowercase();
        (tip.contains("evkey") || tip.contains("unikey") || tip.contains("gõ nhanh")) && (tip.contains("tắt") || tip.contains("bật") || tip.contains("mở"))
    })
}

fn ime_state() -> Option<bool> {
    ime_icon().map(|i| i.tip.to_lowercase().contains("tắt"))
}


/// Label for the layout the user is typing with right now. While the dock itself is in front
/// (the user just clicked it) the previous app's layout is kept.
pub fn label() -> &'static str {
    // SAFETY: plain queries on the foreground window.
    let lang = unsafe {
        let hwnd = GetForegroundWindow();
        let mut pid = 0u32;
        let tid = GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if hwnd.0.is_null() || tid == 0 || pid == std::process::id() {
            LAST.with(Cell::get)
        } else {
            let l = (GetKeyboardLayout(tid).0 as usize & 0xFFFF) as u16;
            LAST.with(|c| c.set(l));
            l
        }
    };
    if let Some(vi) = ime_state() {
        return if vi { "VI" } else { "ENG" };
    }
    match lang & 0x3FF {
        0x09 => "ENG",
        0x2A => "VI",
        0x11 => "JA",
        0x12 => "KO",
        0x04 => "ZH",
        0x0C => "FR",
        0x07 => "DE",
        0x0A => "ES",
        0x19 => "RU",
        _ => "?",
    }
}

/// Switches to the next input language (the Win+Space shortcut).
pub fn switch() {
    // A Vietnamese IME (EVKey/UniKey) toggles with a click on its tray icon.
    if let Some(icon) = ime_icon() {
        if super::traysvc::click(&icon, super::traysvc::Action::Left, 0, 0) {
            return;
        }
    }
    // SAFETY: synthesises a key chord; no pointers involved.
    unsafe {
        keybd_event(VK_LWIN.0 as u8, 0, Default::default(), 0);
        keybd_event(VK_SPACE.0 as u8, 0, Default::default(), 0);
        keybd_event(VK_SPACE.0 as u8, 0, KEYEVENTF_KEYUP, 0);
        keybd_event(VK_LWIN.0 as u8, 0, KEYEVENTF_KEYUP, 0);
    }
}
