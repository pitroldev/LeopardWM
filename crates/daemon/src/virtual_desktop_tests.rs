//! Opt-in regression with real Windows virtual desktops. Moves only a window
//! created by this test; never switches desktops or moves an existing app.

use crate::config::Config;
use crate::state::{AppState, StateSnapshot, WorkspaceSnapshot};
use leopardwm_core_layout::Workspace;
use leopardwm_platform_win32 as platform;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use windows::core::{w, GUID};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Shell::{IVirtualDesktopManager, VirtualDesktopManager};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, DispatchMessageW, GetWindowRect, PeekMessageW, ShowWindow, MSG,
    PM_REMOVE, SW_SHOWNOACTIVATE, WINDOW_EX_STYLE, WS_OVERLAPPEDWINDOW,
};

struct Com;
impl Drop for Com {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

struct Fixture(HWND);
impl Fixture {
    fn new(x: i32) -> Self {
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("LeopardWM isolated native desktop regression"),
                WS_OVERLAPPEDWINDOW,
                x,
                100,
                480,
                320,
                None,
                None,
                None,
                None,
            )
            .unwrap()
        };
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        Self(hwnd)
    }
    fn id(&self) -> u64 {
        self.0 .0 as u64
    }
    fn rect(&self) -> RECT {
        let mut rect = RECT::default();
        unsafe {
            GetWindowRect(self.0, &mut rect).unwrap();
        }
        rect
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}

fn wait_until(mut predicate: impl FnMut() -> bool) {
    wait_until_with_deadline(Duration::from_secs(5), &mut predicate);
}

fn wait_until_with_deadline(timeout: Duration, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    loop {
        let mut msg = MSG::default();
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                DispatchMessageW(&msg);
            }
        }
        if predicate() {
            return;
        }
        assert!(Instant::now() < deadline, "Native desktop did not settle");
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
#[ignore = "creates two disposable windows; requires an existing second Windows virtual desktop"]
fn startup_restore_does_not_import_or_focus_another_native_desktop() {
    assert_eq!(
        std::env::var("LEOPARDWM_TEST_NATIVE_DESKTOPS").as_deref(),
        Ok("1")
    );
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok().unwrap();
    }
    let _com = Com;
    let manager: IVirtualDesktopManager =
        unsafe { CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_ALL).unwrap() };
    // Existing windows are inspected read-only to discover an existing desktop.
    let other_id = platform::collect_all_top_level_window_ids()
        .window_ids
        .into_iter()
        .find_map(|id| unsafe {
            let hwnd = HWND(id as *mut _);
            if manager
                .IsWindowOnCurrentVirtualDesktop(hwnd)
                .ok()?
                .as_bool()
            {
                return None;
            }
            let desktop = manager.GetWindowDesktopId(hwnd).ok()?;
            (desktop != GUID::zeroed()).then_some(desktop)
        })
        .expect("Need an existing foreign desktop containing a window; no desktop is created");
    let local = Fixture::new(100);
    let foreign = Fixture::new(650);
    wait_until(|| unsafe {
        manager
            .GetWindowDesktopId(local.0)
            .is_ok_and(|id| id != GUID::zeroed())
            && manager
                .GetWindowDesktopId(foreign.0)
                .is_ok_and(|id| id != GUID::zeroed())
    });
    let local_id = unsafe { manager.GetWindowDesktopId(local.0).unwrap() };
    assert_ne!(local_id, other_id);
    unsafe {
        manager.MoveWindowToDesktop(foreign.0, &other_id).unwrap();
    }
    wait_until(|| unsafe {
        manager
            .GetWindowDesktopId(foreign.0)
            .is_ok_and(|id| id == other_id)
            && !manager
                .IsWindowOnCurrentVirtualDesktop(foreign.0)
                .unwrap()
                .as_bool()
    });
    assert!(platform::is_window_on_current_desktop(local.id()));
    assert!(!platform::is_window_on_current_desktop(foreign.id()));
    // Demonstrate that the pre-fix restore predicate admitted this exact HWND.
    assert!(platform::is_valid_window(foreign.id()));
    assert!(!platform::is_excluded_window_class_hwnd(foreign.id()));
    assert!(!platform::window_manage_block(foreign.id()).is_blocked());

    let monitors = platform::enumerate_monitors().unwrap();
    let monitor = monitors.iter().find(|m| m.is_primary).unwrap();
    let device = monitor.device_name.clone();
    let mut workspace = Workspace::default();
    workspace.insert_window(local.id(), Some(480)).unwrap();
    workspace.insert_window(foreign.id(), Some(480)).unwrap();
    let snapshot = StateSnapshot {
        saved_at: "0".into(),
        workspaces: vec![WorkspaceSnapshot {
            monitor_device_name: device.clone(),
            workspace_index: 0,
            workspace,
        }],
        focused_monitor_name: device,
        active_workspace: HashMap::new(),
        tab_title_overrides: HashMap::new(),
    };
    let mut config = Config::default();
    config.behavior.disable_snap_layouts = false;
    let mut state = AppState::new_with_config(config, monitors);
    // An inactive LeopardWM workspace uses APP cloak on the same Windows
    // desktop. It must survive restoration even though its pixels are hidden.
    platform::dwm_cloak_window(local.id());
    assert!(platform::is_window_on_current_desktop(local.id()));
    state.restore_workspace_structure(&snapshot);
    assert!(state.find_window_workspace(local.id()).is_some());
    assert!(state.find_window_workspace(foreign.id()).is_none());
    platform::dwm_uncloak_window(local.id());
    let enumerated = platform::enumerate_windows().unwrap();
    assert!(enumerated.iter().any(|w| w.hwnd == local.id()));
    assert!(!enumerated.iter().any(|w| w.hwnd == foreign.id()));

    let rect_before = foreign.rect();
    let foreground_before = platform::get_foreground_window();
    assert!(!platform::set_foreground_window(foreign.id()).unwrap());
    assert_eq!(platform::get_foreground_window(), foreground_before);
    assert_eq!(foreign.rect(), rect_before);
    assert_eq!(
        unsafe { manager.GetWindowDesktopId(foreign.0).unwrap() },
        other_id
    );
    assert!(unsafe {
        manager
            .IsWindowOnCurrentVirtualDesktop(local.0)
            .unwrap()
            .as_bool()
    });
    if let Ok(directory) = std::env::var("LEOPARDWM_TEST_NATIVE_DESKTOP_DIR") {
        // Optional external full-daemon harness. Export only our two disposable
        // HWNDs; the harness owns backup/restoration of the real config/state.
        let directory = std::path::PathBuf::from(directory);
        std::fs::write(
            directory.join("fixture-state.json"),
            serde_json::to_vec(&snapshot).unwrap(),
        )
        .unwrap();
        std::fs::write(
            directory.join("ready.json"),
            serde_json::to_vec(&serde_json::json!({
                "local": local.id(), "foreign": foreign.id(), "pid": std::process::id()
            }))
            .unwrap(),
        )
        .unwrap();
        wait_until_with_deadline(Duration::from_secs(75), || {
            directory.join("check-now").exists()
        });
        assert_eq!(
            foreign.rect(),
            rect_before,
            "Full daemon moved the foreign-desktop window"
        );
        assert_eq!(
            unsafe { manager.GetWindowDesktopId(foreign.0).unwrap() },
            other_id
        );
        assert!(unsafe {
            manager
                .IsWindowOnCurrentVirtualDesktop(local.0)
                .unwrap()
                .as_bool()
        });
        std::fs::write(
            directory.join("verified.json"),
            b"{\"foreign_geometry_preserved\":true,\"desktop_membership_preserved\":true}",
        )
        .unwrap();
    }
    println!("PASS: legacy restore would import foreign HWND; fixed restore and enumeration reject it; focus refused; native desktop and geometry preserved");
}
