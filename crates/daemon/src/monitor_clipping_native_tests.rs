//! Native regression through AppState and its real apply worker; owns one fixture.

use super::*;
use std::time::Duration;
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, DispatchMessageW, GetMessageW, PostThreadMessageW, ShowWindow,
    MSG, SW_SHOWNOACTIVATE, WM_QUIT, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_OVERLAPPEDWINDOW,
};

struct Fixture {
    id: u64,
    thread_id: u32,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = leopardwm_platform_win32::restore_window_region(self.id);
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[test]
#[ignore = "opt-in native fixture; set LEOPARDWM_TEST_MONITOR_CLIPPING=1"]
fn stationary_repair_dispatches_the_unchanged_layout_through_the_daemon_worker() {
    if std::env::var("LEOPARDWM_TEST_MONITOR_CLIPPING").as_deref() != Ok("1") {
        return;
    }
    use windows::Win32::Graphics::Gdi::{CreateRectRgn, DeleteObject, GetWindowRgn, SetWindowRgn};
    let monitors = leopardwm_platform_win32::enumerate_monitors().unwrap();
    let owner = monitors
        .iter()
        .find(|m| m.rect.x < 0 && m.rect.y == 0)
        .unwrap_or(&monitors[0])
        .clone();
    let rect = owner.rect;
    let (tx, rx) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || unsafe {
        use windows::Win32::UI::HiDpi::{
            SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        };
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let hwnd = CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            w!("STATIC"),
            w!("LeopardWM stationary repair fixture"),
            WS_OVERLAPPEDWINDOW,
            rect.x + 100,
            rect.y + 100,
            1000,
            700,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        tx.send((hwnd.0 as usize as u64, GetCurrentThreadId()))
            .unwrap();
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).0 > 0 {
            DispatchMessageW(&message);
        }
        let _ = DestroyWindow(hwnd);
    });
    let (id, thread_id) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let _fixture = Fixture {
        id,
        thread_id,
        thread: Some(thread),
    };
    let hwnd = HWND(id as usize as *mut _);
    let has_region = || unsafe {
        let region = CreateRectRgn(0, 0, 0, 0);
        let kind = GetWindowRgn(hwnd, region);
        let _ = DeleteObject(region.into());
        kind.0 != 0
    };
    let mut config = crate::config::Config::default();
    config.appearance.active_border = false;
    config.behavior.clip_tiled_windows = true;
    config.behavior.swap_chain_ghost_animation = false;
    let mut state = AppState::new_with_config(config, monitors);
    state.workspaces.get_mut(&owner.id).unwrap()[0]
        .insert_window(id, Some(rect.width + 400))
        .unwrap();
    state.workspaces.get_mut(&owner.id).unwrap()[0].stop_animation();
    state.paused = false;
    state.apply_layout().unwrap();
    assert!(has_region(), "fixture must have a partial monitor clip");
    assert!(state.pending_physical_presentations.is_empty());
    assert!(state.last_physical_presentations.contains_key(&id));
    assert!(!state.repair_stationary_monitor_clips().unwrap());
    unsafe { assert_ne!(SetWindowRgn(hwnd, None, true), 0) };
    let before = state.physical_invalidation_id.load(Ordering::SeqCst);
    assert!(state.repair_stationary_monitor_clips().unwrap());
    assert!(state.physical_invalidation_id.load(Ordering::SeqCst) > before);
    assert!(
        has_region(),
        "unchanged layout must dispatch clipping repair"
    );
    assert!(!state.repair_stationary_monitor_clips().unwrap());
    state.paused = true;
}
