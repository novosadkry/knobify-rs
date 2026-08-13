use std::os::raw::c_int;
use std::ptr::null_mut;
use winapi::shared::minwindef::{LPARAM, LRESULT, WPARAM};
use winapi::shared::windef::HHOOK;
use winapi::um::errhandlingapi::GetLastError;
use winapi::um::winuser::{
    CallNextHookEx, GetMessageA, SetWindowsHookExA, HC_ACTION, KBDLLHOOKSTRUCT, WH_KEYBOARD_LL,
    WM_KEYDOWN, WM_SYSKEYDOWN,
};

static mut HOOK: HHOOK = null_mut();
static mut CALLBACK: Option<Box<dyn FnMut(u32)>> = None;

unsafe extern "system" fn raw_callback(code: c_int, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as c_int {
        if wparam as u32 == WM_KEYDOWN || wparam as u32 == WM_SYSKEYDOWN {
            let kb = *(lparam as *const KBDLLHOOKSTRUCT);
            if let Some(callback) = &mut *(&raw mut CALLBACK) {
                callback(kb.vkCode);
            }
        }
    }
    CallNextHookEx(HOOK, code, wparam, lparam)
}

/// Installs a keyboard-only low-level hook (WH_KEYBOARD_LL) and blocks pumping
/// messages for it. Deliberately avoids installing a mouse hook (WH_MOUSE_LL),
/// since a system-wide mouse hook interferes with raw mouse input in games
/// (observed as the cursor flicking to the center of the screen).
pub fn listen<F>(callback: F) -> Result<(), u32>
where
    F: FnMut(u32) + 'static,
{
    unsafe {
        CALLBACK = Some(Box::new(callback));
        let hook = SetWindowsHookExA(WH_KEYBOARD_LL, Some(raw_callback), null_mut(), 0);
        if hook.is_null() {
            return Err(GetLastError());
        }
        HOOK = hook;

        GetMessageA(null_mut(), null_mut(), 0, 0);
    }
    Ok(())
}
