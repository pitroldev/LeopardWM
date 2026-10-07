//! Private shell APIs are confined to the explicit offline import operation.
//! Regular daemon startup never enumerates, moves or removes native desktops.

use anyhow::{anyhow, bail, ensure, Context, Result};
use leopardwm_core_layout::Rect;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use windows::core::w;
use windows::Win32::Foundation::{HANDLE, HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{MonitorFromWindow, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::WindowsAndMessaging::{
    GetPropW, GetWindowPlacement, GetWindowThreadProcessId, IsIconic, SetPropW, SetWindowPlacement,
    WINDOWPLACEMENT, WINDOWPLACEMENT_FLAGS,
};

const TOKEN: windows::core::PCWSTR = w!("LeopardWMNativeDesktopImport");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Desktop {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Placement {
    pub flags: u32,
    pub show_cmd: u32,
    pub min: [i32; 2],
    pub max: [i32; 2],
    pub normal: [i32; 4],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Window {
    pub hwnd: u64,
    pub pid: u32,
    pub tid: u32,
    pub desktop: String,
    pub pinned: bool,
    pub title: String,
    pub class_name: String,
    pub executable: String,
    pub monitor: String,
    pub rect: Rect,
    pub minimized: bool,
    pub manageable: bool,
    pub placement: Placement,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Inventory {
    pub desktops: Vec<Desktop>,
    pub current: String,
    pub windows: Vec<Window>,
}

fn api<T>(result: winvd::Result<T>) -> Result<T> {
    result.map_err(|e| anyhow!("Windows virtual desktop API unavailable: {e:?}"))
}

fn guid(id: &str) -> Result<windows_058::core::GUID> {
    let hex = id.replace(['-', '{', '}'], "");
    ensure!(hex.len() == 32, "Invalid native desktop ID");
    Ok(windows_058::core::GUID::from_u128(u128::from_str_radix(
        &hex, 16,
    )?))
}

pub fn desktops() -> Result<Vec<Desktop>> {
    api(winvd::get_desktops())?
        .into_iter()
        .map(|desktop| {
            Ok(Desktop {
                id: format!("{:?}", api(desktop.get_id())?),
                name: api(desktop.get_name())?,
            })
        })
        .collect()
}

pub fn current_desktop() -> Result<String> {
    Ok(format!(
        "{:?}",
        api(api(winvd::get_current_desktop())?.get_id())?
    ))
}

pub fn window_desktop(hwnd: u64) -> Result<String> {
    let window = windows_058::Win32::Foundation::HWND(hwnd as *mut _);
    Ok(format!(
        "{:?}",
        api(api(winvd::get_desktop_by_window(window))?.get_id())?
    ))
}

fn capture_window(id: u64, desktop: String, monitors: &[crate::MonitorInfo]) -> Result<Window> {
    let hwnd = HWND(id as *mut _);
    let vd_hwnd = windows_058::Win32::Foundation::HWND(id as *mut _);
    let mut pid = 0;
    let tid = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    ensure!(
        pid != 0 && tid != 0,
        "Window disappeared during native desktop inventory"
    );
    let info = crate::get_window_info(id);
    let mut placement = WINDOWPLACEMENT {
        length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    unsafe {
        GetWindowPlacement(hwnd, &mut placement)?;
    }
    let r = placement.rcNormalPosition;
    let rect = info.as_ref().map(|i| i.rect).unwrap_or(Rect::new(
        r.left,
        r.top,
        r.right - r.left,
        r.bottom - r.top,
    ));
    let monitor_id = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) }.0 as isize;
    let monitor = monitors
        .iter()
        .find(|m| m.id == monitor_id)
        .context("Window monitor is unavailable")?;
    Ok(Window {
        hwnd: id,
        pid,
        tid,
        desktop,
        pinned: api(winvd::is_pinned_window(vd_hwnd))? || api(winvd::is_pinned_app(vd_hwnd))?,
        title: info.as_ref().map(|i| i.title.clone()).unwrap_or_default(),
        class_name: info
            .as_ref()
            .map(|i| i.class_name.clone())
            .unwrap_or_default(),
        executable: crate::get_process_executable(pid).unwrap_or_default(),
        monitor: monitor.device_name.clone(),
        rect,
        minimized: unsafe { IsIconic(hwnd).as_bool() },
        manageable: info.is_some() && !crate::window_manage_block(id).is_blocked(),
        placement: Placement {
            flags: placement.flags.0,
            show_cmd: placement.showCmd,
            min: [placement.ptMinPosition.x, placement.ptMinPosition.y],
            max: [placement.ptMaxPosition.x, placement.ptMaxPosition.y],
            normal: [r.left, r.top, r.right, r.bottom],
        },
    })
}

pub fn inventory() -> Result<Inventory> {
    let desktops = desktops()?;
    ensure!(!desktops.is_empty(), "No native desktops found");
    let current = current_desktop()?;
    ensure!(
        desktops.iter().any(|d| d.id == current),
        "Current desktop changed during inventory"
    );
    let monitors = crate::enumerate_monitors()?;
    let collection = crate::collect_all_top_level_window_ids();
    if let Some(error) = collection.error {
        return Err(error.into());
    }
    let mut windows = Vec::new();
    for id in collection.window_ids {
        let native = windows_058::Win32::Foundation::HWND(id as *mut _);
        let desktop = match winvd::get_desktop_by_window(native) {
            Ok(desktop) => format!("{:?}", api(desktop.get_id())?),
            Err(winvd::Error::WindowNotFound | winvd::Error::ComElementNotFound) => continue,
            Err(error) => return Err(anyhow!("Cannot inventory HWND {id}: {error:?}")),
        };
        // Pinned/system views can have a sentinel ID that is not a desktop.
        if !desktops.iter().any(|d| d.id == desktop) {
            continue;
        }
        windows.push(capture_window(id, desktop, &monitors)?);
    }
    Ok(Inventory {
        desktops,
        current,
        windows,
    })
}

pub fn stamp(window: &Window, token: u64) -> Result<()> {
    ensure!(
        same_process(window),
        "Window lifetime changed before migration"
    );
    unsafe {
        SetPropW(
            HWND(window.hwnd as *mut _),
            TOKEN,
            Some(HANDLE(token as *mut _)),
        )?;
    }
    Ok(())
}

pub fn same_process(window: &Window) -> bool {
    let mut pid = 0;
    let tid = unsafe { GetWindowThreadProcessId(HWND(window.hwnd as *mut _), Some(&mut pid)) };
    tid != 0 && tid == window.tid && pid == window.pid
}

pub fn same_lifetime(window: &Window, token: u64) -> bool {
    same_process(window)
        && unsafe { GetPropW(HWND(window.hwnd as *mut _), TOKEN).0 as u64 == token }
}

pub fn move_window(window: &Window, token: u64, expected_source: &str, target: &str) -> Result<()> {
    ensure!(
        same_lifetime(window, token),
        "Window lifetime changed; refusing to move recycled HWND"
    );
    ensure!(
        window_desktop(window.hwnd)? == expected_source,
        "Window changed native desktop during migration"
    );
    let hwnd = windows_058::Win32::Foundation::HWND(window.hwnd as *mut _);
    api(winvd::move_window_to_desktop(guid(target)?, &hwnd))?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        ensure!(
            same_lifetime(window, token),
            "Window lifetime departed while moving"
        );
        if window_desktop(window.hwnd)? == target {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("Windows did not confirm the desktop move");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub fn restore_placement(window: &Window, token: u64) -> Result<()> {
    ensure!(
        same_lifetime(window, token),
        "Cannot restore a replaced HWND"
    );
    let p = &window.placement;
    let native = WINDOWPLACEMENT {
        length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
        flags: WINDOWPLACEMENT_FLAGS(p.flags),
        showCmd: p.show_cmd,
        ptMinPosition: POINT {
            x: p.min[0],
            y: p.min[1],
        },
        ptMaxPosition: POINT {
            x: p.max[0],
            y: p.max[1],
        },
        rcNormalPosition: RECT {
            left: p.normal[0],
            top: p.normal[1],
            right: p.normal[2],
            bottom: p.normal[3],
        },
    };
    unsafe {
        SetWindowPlacement(HWND(window.hwnd as *mut _), &native)?;
    }
    Ok(())
}

pub fn remove_empty_desktop(id: &str, fallback: &str) -> Result<bool> {
    ensure!(
        id != fallback && current_desktop()? == fallback,
        "Native desktop changed; refusing removal"
    );
    let live = inventory()?;
    if live.windows.iter().any(|w| w.desktop == id && !w.pinned) {
        return Ok(false);
    }
    api(winvd::remove_desktop(guid(id)?, guid(fallback)?))?;
    ensure!(
        !desktops()?.iter().any(|d| d.id == id),
        "Windows did not confirm desktop removal"
    );
    Ok(true)
}

/// Recovery recreates missing native desktops by name. Windows assigns new IDs;
/// order/wallpaper are not reconstructed, and existing desktops are never deleted.
pub fn create_desktop(name: &str) -> Result<String> {
    let desktop = api(winvd::create_desktop())?;
    api(desktop.set_name(name))?;
    Ok(format!("{:?}", api(desktop.get_id())?))
}
