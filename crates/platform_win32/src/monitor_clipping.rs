//! Native monitor clipping, independent of application geometry.
//!
//! Region coordinates are outer-window local coordinates, mirrored for RTL.
//! The original state is journaled before any mutation so a separate watchdog
//! can restore it. Windows owns each HRGN after a successful SetWindowRgn.

use crate::{window_id_to_hwnd, PlatformConfig, Win32Error};
use leopardwm_core_layout::{Rect, WindowId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::c_void;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use windows::core::w;
use windows::Win32::Foundation::{GetLastError, SetLastError, HANDLE, HWND, RECT, WIN32_ERROR};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DwmSetWindowAttribute, DWMWA_SYSTEMBACKDROP_TYPE,
};
use windows::Win32::Graphics::Gdi::{
    CombineRgn, CreateRectRgn, DeleteObject, GetRegionData, GetWindowRgn, SetWindowRgn, ERROR,
    HRGN, RGNDATA, RGNDATAHEADER, RGN_AND, RGN_OR,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetPropW, GetWindowLongW, GetWindowRect, GetWindowThreadProcessId, IsWindow, RemovePropW,
    SetPropW, GWL_EXSTYLE, WS_EX_LAYOUTRTL,
};

const PROPERTY: windows::core::PCWSTR = w!("LeopardWMMonitorClipToken");
const MAX_RECTS: usize = 16_384;
const MAX_JOURNAL_BYTES: u64 = 2 * 1024 * 1024;
type Insets = (i32, i32, i32, i32);
// None: absent; Some([]): explicitly empty. Never confuse the two.
type Region = Option<Vec<[i32; 4]>>;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Original {
    version: u32,
    window_id: WindowId,
    token: u64,
    process_id: u32,
    thread_id: u32,
    region: Region,
    region_width: i32,
    rtl: bool,
    insets: Insets,
    backdrop: Option<i32>,
}

#[derive(Clone, Debug)]
struct OwnedClip {
    original: Original,
    installed: Region,
    installed_width: i32,
}

static CLIPS: LazyLock<Mutex<HashMap<WindowId, OwnedClip>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
// Read-side geometry must remain available while SetWindowRgn is in progress.
// Do not make the daemon event loop wait on a synchronous foreign app call.
static GEOMETRY: LazyLock<Mutex<HashMap<WindowId, (u64, Insets)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static SEQUENCE: AtomicU64 = AtomicU64::new(1);

fn error(message: impl std::fmt::Display) -> Win32Error {
    Win32Error::SetPositionFailed(format!("monitor clipping: {message}"))
}

fn journal_path(id: WindowId) -> Result<PathBuf, Win32Error> {
    let root = std::env::var_os("LOCALAPPDATA").ok_or_else(|| error("LOCALAPPDATA missing"))?;
    Ok(PathBuf::from(root)
        .join("leopardwm")
        .join("monitor-clipping")
        .join(format!("{id:x}.json")))
}

fn write_journal(original: &Original) -> Result<(), Win32Error> {
    let path = journal_path(original.window_id)?;
    std::fs::create_dir_all(path.parent().unwrap()).map_err(error)?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let bytes = serde_json::to_vec(original).map_err(error)?;
    let mut file = std::fs::File::create(&temporary).map_err(error)?;
    file.write_all(&bytes).map_err(error)?;
    file.sync_all().map_err(error)?;
    drop(file);
    std::fs::rename(temporary, path).map_err(error)
}

fn remove_journal(id: WindowId) {
    if let Ok(path) = journal_path(id) {
        if let Err(err) = std::fs::remove_file(path) {
            if err.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(window_id = id, %err, "Could not remove clipping recovery journal");
            }
        }
    }
}

fn read_journal(id: WindowId) -> Result<Option<Original>, Win32Error> {
    let path = journal_path(id)?;
    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(error(err)),
    };
    if metadata.len() > MAX_JOURNAL_BYTES {
        return Err(error("oversized recovery journal"));
    }
    let original: Original =
        serde_json::from_slice(&std::fs::read(path).map_err(error)?).map_err(error)?;
    if original.version != 2
        || original.window_id != id
        || original.token == 0
        || original.region_width <= 0
        || original
            .region
            .as_ref()
            .is_some_and(|r| r.len() > MAX_RECTS)
    {
        return Err(error("invalid recovery journal"));
    }
    Ok(Some(original))
}

fn outer_rect(id: WindowId) -> Result<Rect, Win32Error> {
    let hwnd = window_id_to_hwnd(id)?;
    let mut rect = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect) }.map_err(error)?;
    Ok(Rect::new(
        rect.left,
        rect.top,
        rect.right - rect.left,
        rect.bottom - rect.top,
    ))
}

/// Clip a monitor rectangle in window-local coordinates. Clamp each edge before
/// subtraction, including fully off-screen windows and negative desktop origins.
fn local_clip(outer: Rect, monitor: Rect, rtl: bool) -> [i32; 4] {
    let width = i64::from(outer.width.max(0));
    let height = i64::from(outer.height.max(0));
    let left = (i64::from(monitor.x) - i64::from(outer.x)).clamp(0, width);
    let right =
        (i64::from(monitor.x) + i64::from(monitor.width) - i64::from(outer.x)).clamp(0, width);
    let top = (i64::from(monitor.y) - i64::from(outer.y)).clamp(0, height);
    let bottom =
        (i64::from(monitor.y) + i64::from(monitor.height) - i64::from(outer.y)).clamp(0, height);
    if rtl {
        [
            (width - right) as i32,
            top as i32,
            (width - left) as i32,
            bottom as i32,
        ]
    } else {
        [left as i32, top as i32, right as i32, bottom as i32]
    }
}

fn intersect(a: [i32; 4], b: [i32; 4]) -> [i32; 4] {
    let left = a[0].max(b[0]);
    let top = a[1].max(b[1]);
    [left, top, a[2].min(b[2]).max(left), a[3].min(b[3]).max(top)]
}

fn apply_baseline(base: &Region, clip: [i32; 4]) -> Region {
    Some(match base {
        None => vec![clip],
        Some(rects) => rects
            .iter()
            .map(|r| intersect(*r, clip))
            .filter(|r| r[0] < r[2] && r[1] < r[3])
            .collect(),
    })
}

/// USER32 shifts a mirrored HWND's existing region by the width delta when it
/// resizes. This is native anchoring, not an app replacing its region.
fn region_at_width(region: &Region, prior_width: i32, width: i32, rtl: bool) -> Region {
    let delta = if rtl {
        width.saturating_sub(prior_width)
    } else {
        0
    };
    region.as_ref().map(|rects| {
        rects
            .iter()
            .map(|r| {
                [
                    r[0].saturating_add(delta),
                    r[1],
                    r[2].saturating_add(delta),
                    r[3],
                ]
            })
            .collect()
    })
}

fn is_rtl(hwnd: HWND) -> bool {
    unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_LAYOUTRTL.0 != 0 }
}

fn full_clip(outer: Rect, clip: [i32; 4]) -> bool {
    clip == [0, 0, outer.width, outer.height]
}

struct OwnedRegion(HRGN);
impl Drop for OwnedRegion {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.0.into());
        }
    }
}

fn make_region(rects: &[[i32; 4]]) -> Result<OwnedRegion, Win32Error> {
    let region = OwnedRegion(unsafe { CreateRectRgn(0, 0, 0, 0) });
    if region.0.is_invalid() {
        return Err(error("CreateRectRgn failed"));
    }
    for r in rects {
        let part = OwnedRegion(unsafe { CreateRectRgn(r[0], r[1], r[2], r[3]) });
        if part.0.is_invalid()
            || unsafe { CombineRgn(Some(region.0), Some(region.0), Some(part.0), RGN_OR) }.0
                == ERROR
        {
            return Err(error("could not combine original region"));
        }
    }
    Ok(region)
}

fn query_region(hwnd: HWND) -> Result<Region, Win32Error> {
    let region = make_region(&[])?;
    // GetWindowRgn's documented ERROR is ambiguous. Nonzero last-error aborts;
    // zero last-error on a live accessible window is our explicit absent-region
    // compatibility policy, not an assertion that the API proves absence.
    let previous = unsafe { GetLastError() };
    unsafe {
        SetLastError(WIN32_ERROR(0));
    }
    let kind = unsafe { GetWindowRgn(hwnd, region.0) };
    let last = unsafe { GetLastError() };
    unsafe {
        SetLastError(previous);
    }
    if kind.0 == ERROR {
        if last.0 != 0 || unsafe { !IsWindow(Some(hwnd)).as_bool() } {
            return Err(error(format!("GetWindowRgn failed ({})", last.0)));
        }
        return Ok(None);
    }
    region_data(region.0)
}

fn install_region(hwnd: HWND, region: &Region) -> Result<(), Win32Error> {
    let owned = region
        .as_ref()
        .map(|rects| make_region(rects))
        .transpose()?;
    let handle = owned.as_ref().map(|r| r.0);
    if unsafe { SetWindowRgn(hwnd, handle, true) } == 0 {
        return Err(error("SetWindowRgn failed"));
    }
    if let Some(owned) = owned {
        std::mem::forget(owned);
    }
    Ok(())
}

fn identity_matches(original: &Original) -> bool {
    let Ok(hwnd) = window_id_to_hwnd(original.window_id) else {
        return false;
    };
    unsafe {
        let mut pid = 0;
        IsWindow(Some(hwnd)).as_bool()
            && GetWindowThreadProcessId(hwnd, Some(&mut pid)) == original.thread_id
            && pid == original.process_id
            && GetPropW(hwnd, PROPERTY).0 as usize as u64 == original.token
    }
}

fn backdrop(hwnd: HWND) -> Option<i32> {
    let mut value = 0_i32;
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            (&mut value as *mut i32).cast(),
            4,
        )
    }
    .ok()?;
    Some(value)
}

fn set_backdrop(hwnd: HWND, value: Option<i32>) -> Result<(), Win32Error> {
    if let Some(value) = value {
        unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_SYSTEMBACKDROP_TYPE,
                (&value as *const i32).cast(),
                4,
            )
        }
        .map_err(error)?;
    }
    Ok(())
}

fn capture(id: WindowId, insets: Insets) -> Result<OwnedClip, Win32Error> {
    let hwnd = window_id_to_hwnd(id)?;
    let mut pid = 0;
    let tid = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if tid == 0 {
        return Err(Win32Error::WindowNotFound(id));
    }
    if unsafe { !GetPropW(hwnd, PROPERTY).0.is_null() } {
        return Err(error("unrecovered prior clipping ownership"));
    }
    let region = query_region(hwnd)?;
    let region_width = outer_rect(id)?.width;
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(error)?
        .as_nanos() as u64;
    let token =
        (time ^ (u64::from(std::process::id()) << 32) ^ SEQUENCE.fetch_add(1, Ordering::Relaxed))
            .max(1);
    let original = Original {
        version: 2,
        window_id: id,
        token,
        process_id: pid,
        thread_id: tid,
        region: region.clone(),
        region_width,
        rtl: is_rtl(hwnd),
        insets,
        backdrop: backdrop(hwnd),
    };
    // Journal first. A crash before stamping leaves an unmatched inert record.
    write_journal(&original)?;
    unsafe { SetPropW(hwnd, PROPERTY, Some(HANDLE(token as *mut c_void))) }.map_err(error)?;
    GEOMETRY
        .lock()
        .unwrap_or_else(crate::recover_poisoned_mutex)
        .insert(id, (token, insets));
    Ok(OwnedClip {
        original,
        installed: region,
        installed_width: region_width,
    })
}

fn install_clip(
    owned: &mut OwnedClip,
    clip: [i32; 4],
    retain_installed: bool,
) -> Result<(), Win32Error> {
    if !identity_matches(&owned.original) {
        return Err(error("window lifetime changed"));
    }
    let hwnd = window_id_to_hwnd(owned.original.window_id)?;
    let width = outer_rect(owned.original.window_id)?.width;
    let rtl = is_rtl(hwnd);
    let current = query_region(hwnd)?;
    let expected = region_at_width(
        &owned.installed,
        owned.installed_width,
        width,
        owned.original.rtl,
    );
    if current != expected || rtl != owned.original.rtl {
        // Apps may replace their own region after resizing. Adopt the observed
        // region, never compound successive monitor clips into the baseline.
        owned.original.region = current.clone();
        owned.original.region_width = width;
        owned.original.rtl = rtl;
        write_journal(&owned.original)?;
        owned.installed = current.clone();
        owned.installed_width = width;
    }
    let base = region_at_width(
        &owned.original.region,
        owned.original.region_width,
        width,
        rtl,
    );
    let desired = apply_baseline(&base, clip);
    let region = make_region(desired.as_deref().unwrap_or_default())?;
    if retain_installed {
        if let Some(rects) = &current {
            let prior = make_region(rects)?;
            if unsafe { CombineRgn(Some(region.0), Some(region.0), Some(prior.0), RGN_AND) }.0
                == ERROR
            {
                return Err(error("could not retain pending clipping intersection"));
            }
        }
    }
    // Canonicalize union rectangles so GetWindowRgn comparisons remain stable.
    let desired = region_data(region.0)?;
    if desired != current {
        set_backdrop(hwnd, owned.original.backdrop.map(|_| 1))?; // DWMSBT_NONE
        install_region(hwnd, &desired)?;
    }
    owned.installed = desired;
    owned.installed_width = width;
    Ok(())
}

fn region_data(region: HRGN) -> Result<Region, Win32Error> {
    let bytes = unsafe { GetRegionData(region, 0, None) } as usize;
    let header_size = std::mem::size_of::<RGNDATAHEADER>();
    if bytes < header_size || bytes > header_size + MAX_RECTS * std::mem::size_of::<RECT>() {
        return Err(error("invalid combined region data size"));
    }
    // u64 storage keeps RGNDATAHEADER aligned; returned byte count is validated.
    let mut aligned = vec![0_u64; bytes.div_ceil(8)];
    let data = aligned.as_mut_ptr().cast::<RGNDATA>();
    if unsafe { GetRegionData(region, bytes as u32, Some(data)) } as usize != bytes {
        return Err(error("combined region query failed"));
    }
    let header = unsafe { &(*data).rdh };
    let count = header.nCount as usize;
    if header.dwSize as usize != header_size
        || count > MAX_RECTS
        || header_size + count * std::mem::size_of::<RECT>() != bytes
    {
        return Err(error("invalid combined region rectangle count"));
    }
    let rects = unsafe {
        std::slice::from_raw_parts(
            aligned
                .as_ptr()
                .cast::<u8>()
                .add(header_size)
                .cast::<RECT>(),
            count,
        )
    };
    Ok(Some(
        rects
            .iter()
            .map(|r| [r.left, r.top, r.right, r.bottom])
            .collect(),
    ))
}

/// Tighten before moving. Each intermediate region is safe at both endpoints,
/// even when old asynchronous positioning is still queued on the app thread.
pub(crate) fn prepare_clips(
    targets: &HashMap<WindowId, (Rect, Insets)>,
    config: &PlatformConfig,
    pending: &std::collections::HashSet<WindowId>,
) -> Result<(), Win32Error> {
    let mut clips = CLIPS.lock().unwrap_or_else(crate::recover_poisoned_mutex);
    for (&id, &(target, insets)) in targets {
        let Some(&monitor) = config.clip_owners.get(&id) else {
            continue;
        };
        let hwnd = window_id_to_hwnd(id)?;
        let current = outer_rect(id)?;
        let rtl = is_rtl(hwnd);
        let old_clip = local_clip(current, monitor, rtl);
        let new_clip = local_clip(target, monitor, rtl);
        if full_clip(current, old_clip) && full_clip(target, new_clip) && !clips.contains_key(&id) {
            continue;
        }
        if clips
            .get(&id)
            .is_some_and(|c| !identity_matches(&c.original))
        {
            clips.remove(&id);
            GEOMETRY
                .lock()
                .unwrap_or_else(crate::recover_poisoned_mutex)
                .remove(&id);
            remove_journal(id);
        }
        if let std::collections::hash_map::Entry::Vacant(entry) = clips.entry(id) {
            entry.insert(capture(id, insets)?);
        }
        install_clip(
            clips.get_mut(&id).unwrap(),
            intersect(old_clip, new_clip),
            pending.contains(&id),
        )?;
    }
    Ok(())
}

/// Relax only after a confirmed synchronous move. Pending asynchronous moves
/// keep the intersection installed above, so a late move cannot expose bleed.
pub(crate) fn finish_clips(
    config: &PlatformConfig,
    pending: &std::collections::HashSet<WindowId>,
) -> Result<(), Win32Error> {
    let ids: Vec<_> = CLIPS
        .lock()
        .unwrap_or_else(crate::recover_poisoned_mutex)
        .keys()
        .copied()
        .collect();
    for id in ids {
        if pending.contains(&id) {
            continue;
        }
        let Some(&monitor) = config.clip_owners.get(&id) else {
            restore_window_region(id)?;
            continue;
        };
        let hwnd = window_id_to_hwnd(id)?;
        if crate::is_window_maximized(id) {
            restore_window_region(id)?;
            continue;
        }
        let current = match outer_rect(id) {
            Ok(rect) => rect,
            Err(_) => {
                restore_window_region(id)?;
                continue;
            }
        };
        let rtl = is_rtl(hwnd);
        let clip = local_clip(current, monitor, rtl);
        if full_clip(current, clip) {
            restore_window_region(id)?;
        } else {
            let mut clips = CLIPS.lock().unwrap_or_else(crate::recover_poisoned_mutex);
            if let Some(owned) = clips.get_mut(&id) {
                install_clip(owned, clip, false)?;
            }
        }
    }
    Ok(())
}

/// Original geometry used by placement measurement while DWM frame presentation
/// is changed by region clipping. This does not report the clipped slice width.
pub(crate) fn saved_insets(id: WindowId) -> Option<Insets> {
    let (token, insets) = *GEOMETRY
        .lock()
        .unwrap_or_else(crate::recover_poisoned_mutex)
        .get(&id)?;
    let hwnd = window_id_to_hwnd(id).ok()?;
    (unsafe {
        IsWindow(Some(hwnd)).as_bool() && GetPropW(hwnd, PROPERTY).0 as usize as u64 == token
    })
    .then_some(insets)
}

fn region_is_contained(region: &Region, bounds: [i32; 4]) -> bool {
    region.as_ref().is_some_and(|rects| {
        rects.iter().all(|r| {
            r[0] >= bounds[0] && r[1] >= bounds[1] && r[2] <= bounds[2] && r[3] <= bounds[3]
        })
    })
}

/// Read-only health check for stationary windows. Region updates do not have to
/// change native geometry, so the daemon's layout fast path cannot detect them.
/// This deliberately avoids CLIPS, whose mutation lock can be held by a foreign
/// application's synchronous SetWindowRgn call.
pub fn monitor_clip_repair_needed(config: &PlatformConfig) -> Result<bool, Win32Error> {
    let owned: HashMap<_, _> = GEOMETRY
        .lock()
        .unwrap_or_else(crate::recover_poisoned_mutex)
        .iter()
        .map(|(&id, &(token, _))| (id, token))
        .collect();
    if owned.keys().any(|id| !config.clip_owners.contains_key(id)) {
        return Ok(true);
    }
    for (&id, &monitor) in &config.clip_owners {
        let hwnd = window_id_to_hwnd(id)?;
        if unsafe { !IsWindow(Some(hwnd)).as_bool() } {
            continue;
        }
        let stamped = owned
            .get(&id)
            .is_some_and(|&token| unsafe { GetPropW(hwnd, PROPERTY).0 as usize as u64 == token });
        if crate::is_window_maximized(id) {
            if stamped {
                return Ok(true);
            }
            continue;
        }
        let outer = outer_rect(id)?;
        let bounds = local_clip(outer, monitor, is_rtl(hwnd));
        if full_clip(outer, bounds) {
            if stamped {
                return Ok(true);
            }
        } else if !region_is_contained(&query_region(hwnd)?, bounds) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn restore_window_region(id: WindowId) -> Result<(), Win32Error> {
    let mut clips = CLIPS.lock().unwrap_or_else(crate::recover_poisoned_mutex);
    let saved = clips.get(&id).cloned();
    let original = match if let Some(saved) = &saved {
        Some(saved.original.clone())
    } else {
        read_journal(id)?
    } {
        Some(original) => original,
        None => return Ok(()),
    };
    if identity_matches(&original) {
        let hwnd = window_id_to_hwnd(id)?;
        let width = outer_rect(id)?.width;
        let current = query_region(hwnd)?;
        let app_changed_region = saved.as_ref().is_some_and(|c| {
            current != region_at_width(&c.installed, c.installed_width, width, c.original.rtl)
        });
        if !app_changed_region {
            install_region(
                hwnd,
                &region_at_width(&original.region, original.region_width, width, original.rtl),
            )?;
        }
        set_backdrop(hwnd, original.backdrop)?;
        unsafe { RemovePropW(hwnd, PROPERTY) }.map_err(error)?;
    }
    clips.remove(&id);
    GEOMETRY
        .lock()
        .unwrap_or_else(crate::recover_poisoned_mutex)
        .remove(&id);
    remove_journal(id);
    Ok(())
}

/// Also works in the watchdog process: journal plus HWND property are sufficient.
pub fn restore_all_window_regions() -> Result<(), Win32Error> {
    let directory = journal_path(0)?.parent().unwrap().to_owned();
    let files = match std::fs::read_dir(directory) {
        Ok(files) => files,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(error(err)),
    };
    let mut failures = Vec::new();
    for entry in files.take(10_000) {
        let entry = entry.map_err(error)?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Some(id) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| u64::from_str_radix(s, 16).ok())
        else {
            continue;
        };
        if let Err(err) = restore_window_region(id) {
            failures.push(err.to_string());
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(crate::combine_operation_failures(
            "region recovery",
            failures,
        ))
    }
}

#[cfg(test)]
#[path = "monitor_clipping_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "monitor_clipping_chrome_tests.rs"]
mod chrome_tests;
