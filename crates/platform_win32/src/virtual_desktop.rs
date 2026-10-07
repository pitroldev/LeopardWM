//! Read-only Windows virtual-desktop membership. This is not a desktop bridge:
//! no production path moves windows between Windows desktops.

use std::cell::RefCell;
use windows::core::GUID;
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Shell::{IVirtualDesktopManager, VirtualDesktopManager};

struct DesktopManager {
    manager: Option<IVirtualDesktopManager>,
    uninitialize: bool,
}

impl DesktopManager {
    fn new() -> windows::core::Result<Self> {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr.is_err() && hr != RPC_E_CHANGED_MODE {
            return Err(hr.into());
        }
        let mut context = Self {
            manager: None,
            uninitialize: hr.is_ok(),
        };
        context.manager =
            Some(unsafe { CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_ALL)? });
        Ok(context)
    }
}

impl Drop for DesktopManager {
    fn drop(&mut self) {
        // Release the apartment-bound interface before balancing COM startup.
        self.manager.take();
        if self.uninitialize {
            unsafe { CoUninitialize() };
        }
    }
}

thread_local! {
    static MANAGER: RefCell<Option<DesktopManager>> = const { RefCell::new(None) };
}

/// Whether a window may be admitted/restored/focused on the current Windows
/// desktop. Shell cloak is checked separately: our own DWM cloak must not reject
/// saved, inactive LeopardWM workspaces on the *same* native desktop.
pub fn is_window_on_current_desktop(window_id: u64) -> bool {
    let Ok(hwnd) = crate::window_id_to_hwnd(window_id) else {
        return false;
    };
    if !crate::is_valid_window(window_id) || crate::is_window_shell_cloaked(window_id) {
        return false;
    }
    let result = MANAGER.with(|slot| -> windows::core::Result<bool> {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(DesktopManager::new()?);
        }
        let manager = slot.as_ref().unwrap().manager.as_ref().unwrap();
        let result = unsafe {
            manager
                .IsWindowOnCurrentVirtualDesktop(hwnd)
                .and_then(|current| {
                    if current.as_bool() {
                        Ok(true)
                    } else {
                        // Shell registration lags window creation; GUID_NULL is an
                        // unassigned window, not evidence of a foreign desktop.
                        manager
                            .GetWindowDesktopId(hwnd)
                            .map(|id| id == GUID::zeroed())
                    }
                })
        };
        if result.is_err() {
            // Explorer may have restarted. Reconnect on the next query.
            *slot = None;
        }
        result
    });
    match result {
        Ok(current) => current,
        Err(error) => {
            tracing::debug!(window_id, %error, "Cannot verify native desktop; deferring window");
            false
        }
    }
}
