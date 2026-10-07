//! Opt-in, read-only audit of a running desktop daemon's real managed windows.
//! Uses production IPC ownership and USER32 region readback; no input injection.
//! A separate opt-in fault test targets an identified disposable Chrome fixture.

use crate::ipc_client::send_command;
use anyhow::{bail, ensure, Context, Result};
use leopardwm_ipc::{IpcCommand, IpcResponse, WindowInfo};
use leopardwm_platform_win32::MonitorInfo;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{GetLastError, SetLastError, HWND, RECT, WIN32_ERROR};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::Graphics::Gdi::{CreateRectRgn, DeleteObject, GetRgnBox, GetWindowRgn};
use windows::Win32::UI::HiDpi::{
    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetGUIThreadInfo, GetWindowLongW, GetWindowRect, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, IsZoomed, GUITHREADINFO, GWL_EXSTYLE, WS_EX_LAYOUTRTL,
};

fn native_sample(
    window: &WindowInfo,
    monitor: &MonitorInfo,
    monitors: &[MonitorInfo],
) -> Result<Option<Value>> {
    let hwnd = HWND(window.window_id as usize as *mut _);
    unsafe {
        let mut pid = 0;
        let tid = GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 || pid != window.process_id {
            return Ok(None);
        }
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return Ok(None);
        }
        let mut cloaked: u32 = 0;
        DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, (&mut cloaked as *mut u32).cast(), 4)?;
        if cloaked != 0 {
            return Ok(None);
        }
        let mut outer = RECT::default();
        GetWindowRect(hwnd, &mut outer)?;
        let maximized = IsZoomed(hwnd).as_bool();
        let mut gui = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        GetGUIThreadInfo(tid, &mut gui)?;
        let moving = gui.hwndMoveSize == hwnd;
        let mut frame = RECT::default();
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&mut frame as *mut RECT).cast(),
            std::mem::size_of::<RECT>() as u32,
        )?;
        let region = CreateRectRgn(0, 0, 0, 0);
        ensure!(!region.is_invalid(), "CreateRectRgn failed");
        SetLastError(WIN32_ERROR(0));
        let kind = GetWindowRgn(hwnd, region).0;
        let last_error = GetLastError().0;
        let mut bounds = RECT::default();
        let box_kind = if kind != 0 {
            GetRgnBox(region, &mut bounds).0
        } else {
            0
        };
        let _ = DeleteObject(region.into());
        let mut after_pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut after_pid));
        if after_pid != pid {
            return Ok(None); // The enumerated lifetime departed during readback.
        }
        ensure!(
            kind != 0 || last_error == 0,
            "GetWindowRgn failed: {last_error}; hwnd={} pid={pid} outer={outer:?}",
            window.window_id
        );
        ensure!(kind == 0 || box_kind != 0, "GetRgnBox failed");
        let mut after = RECT::default();
        GetWindowRect(hwnd, &mut after)?;
        if outer != after {
            return Ok(None); // Avoid combining readbacks from different animation positions.
        }
        let empty = kind == 1;
        let rtl = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_LAYOUTRTL.0 != 0;
        let mut effective = if kind == 0 {
            [outer.left, outer.top, outer.right, outer.bottom]
        } else if rtl {
            [
                outer.right - bounds.right,
                outer.top + bounds.top,
                outer.right - bounds.left,
                outer.top + bounds.bottom,
            ]
        } else {
            [
                outer.left + bounds.left,
                outer.top + bounds.top,
                outer.left + bounds.right,
                outer.top + bounds.bottom,
            ]
        };
        // Invisible resize margins are not compositor content. A parked HWND
        // outside the complete desktop is valid even without an explicit region.
        effective[0] = effective[0].max(frame.left);
        effective[1] = effective[1].max(frame.top);
        effective[2] = effective[2].min(frame.right);
        effective[3] = effective[3].min(frame.bottom);
        let owner = [
            monitor.rect.x,
            monitor.rect.y,
            monitor.rect.x + monitor.rect.width,
            monitor.rect.y + monitor.rect.height,
        ];
        let contained = empty
            || (effective[0] >= owner[0]
                && effective[1] >= owner[1]
                && effective[2] <= owner[2]
                && effective[3] <= owner[3]);
        let crosses = outer.left < owner[0]
            || outer.top < owner[1]
            || outer.right > owner[2]
            || outer.bottom > owner[3];
        let invades: Vec<_> = monitors
            .iter()
            .filter(|other| other.id != monitor.id)
            .filter(|other| {
                !empty
                    && effective[0].max(other.rect.x)
                        < effective[2].min(other.rect.x + other.rect.width)
                    && effective[1].max(other.rect.y)
                        < effective[3].min(other.rect.y + other.rect.height)
            })
            .map(|other| other.device_name.as_str())
            .collect();
        Ok(Some(json!({
            "hwnd": window.window_id, "pid": pid, "application": window.executable,
            "monitor": monitor.device_name, "scale": monitor.scale_factor,
            "outer": [outer.left, outer.top, outer.right, outer.bottom],
            "effective": effective, "owner": owner, "region_kind": kind,
            "frame": [frame.left, frame.top, frame.right, frame.bottom],
            "native_maximized": maximized, "floating": window.is_floating,
            "native_moving": moving,
            "invades": invades,
            "empty": empty, "partial_clip": crosses && !empty && kind != 0,
            "content_clip": kind != 0 && !empty &&
                ((outer.right - outer.left) - (bounds.right - bounds.left) > 32 ||
                 (outer.bottom - outer.top) - (bounds.bottom - bounds.top) > 32),
            "contained": contained,
        })))
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "live desktop read-only audit; requires LEOPARDWM_TEST_LIVE_CLIPPING=1"]
async fn running_daemon_keeps_real_windows_inside_their_owner_monitor() -> Result<()> {
    ensure!(
        std::env::var("LEOPARDWM_TEST_LIVE_CLIPPING").as_deref() == Ok("1"),
        "explicit live desktop audit opt-in required"
    );
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let report = std::env::var_os("LEOPARDWM_TEST_CLIP_REPORT").context("report path required")?;
    let samples = std::env::var("LEOPARDWM_TEST_CLIP_SAMPLES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(100)
        .clamp(1, 1200);
    let monitors = leopardwm_platform_win32::enumerate_monitors()?;
    ensure!(monitors.len() >= 2, "multiple connected monitors required");
    let mut observed = BTreeMap::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut violations = Vec::new();
    let mut checked = 0;
    let mut partial = 0;
    let mut content_partial = 0;
    let mut moving = 0;
    let mut x_ranges: BTreeMap<u64, (i64, i64)> = BTreeMap::new();
    let mut read_errors = Vec::new();
    let started = Instant::now();
    for sample in 0..samples {
        let response = send_command(IpcCommand::QueryAllWindows).await?;
        let IpcResponse::WindowList { windows } = response else {
            bail!("daemon did not return window ownership: {response:?}");
        };
        for window in windows {
            let monitor = monitors
                .iter()
                .find(|m| m.id as i64 == window.monitor_id)
                .context("window references an unknown monitor")?;
            let value = match native_sample(&window, monitor, &monitors) {
                Ok(value) => value,
                Err(error) => {
                    if read_errors.len() < 100 {
                        read_errors.push(json!({"sample": sample, "hwnd": window.window_id,
                            "pid": window.process_id, "monitor": monitor.device_name,
                            "error": error.to_string()}));
                    }
                    continue;
                }
            };
            if let Some(mut value) = value {
                value["sample"] = json!(sample);
                value["elapsed_ms"] = json!(started.elapsed().as_millis());
                checked += 1;
                partial += usize::from(value["partial_clip"] == true);
                content_partial += usize::from(value["content_clip"] == true);
                moving += usize::from(value["native_moving"] == true);
                let x = value["outer"][0].as_i64().unwrap();
                x_ranges
                    .entry(window.window_id)
                    .and_modify(|range| {
                        range.0 = range.0.min(x);
                        range.1 = range.1.max(x);
                    })
                    .or_insert((x, x));
                *counts.entry(window.executable).or_default() += 1;
                if !value["invades"].as_array().unwrap().is_empty()
                    && value["floating"] == false
                    && value["native_maximized"] == false
                    && value["native_moving"] == false
                    && violations.len() < 100
                {
                    violations.push(value.clone());
                }
                observed.insert(window.window_id, value);
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let output = json!({
        "samples": samples, "elapsed_ms": started.elapsed().as_millis(),
        "window_samples": checked, "partial_clip_samples": partial,
        "content_clip_samples": content_partial, "moving_samples": moving,
        "x_ranges": x_ranges,
        "windows_moved_over_50px": x_ranges.values().filter(|(min, max)| max - min > 50).count(),
        "application_samples": counts, "violations": violations,
        "read_errors": read_errors,
        "last_observations": observed.values().collect::<Vec<_>>(),
        "limits": "Native region readback; does not verify compositor pixels or exempt floating/maximized windows.",
    });
    std::fs::write(report, serde_json::to_vec_pretty(&output)?)?;
    eprintln!("native-live-audit samples={samples} window_samples={checked} partial={partial} violations={} apps={counts:?}", violations.len());
    ensure!(
        violations.is_empty(),
        "native monitor containment failed; see private report"
    );
    ensure!(
        read_errors.is_empty(),
        "native readback errors; see private report"
    );
    ensure!(
        checked > 0 && content_partial > 0,
        "no tiled windows with more than 32px clipped observed; coverage incomplete"
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "removes a clip from an explicitly identified disposable Chrome fixture"]
async fn running_daemon_repairs_a_replaced_fixture_region() -> Result<()> {
    ensure!(
        std::env::var("LEOPARDWM_TEST_LIVE_CLIPPING").as_deref() == Ok("1"),
        "opt-in required"
    );
    let tag = std::env::var("LEOPARDWM_TEST_CLIP_FIXTURE_TAG")
        .context("unique fixture title required")?;
    ensure!(
        tag.starts_with("LeopardWM monitor clipping retest ") && tag.len() > 50,
        "unique fixture title required"
    );
    let pids: Vec<u32> = std::env::var("LEOPARDWM_TEST_CLIP_FIXTURE_PIDS")
        .context("owned fixture PIDs required")?
        .split(',')
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()?;
    let report = std::env::var_os("LEOPARDWM_TEST_CLIP_REPORT").context("report path required")?;
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let monitors = leopardwm_platform_win32::enumerate_monitors()?;
    let IpcResponse::WindowList { windows } = send_command(IpcCommand::QueryAllWindows).await?
    else {
        bail!("window ownership unavailable")
    };
    let candidate = windows
        .into_iter()
        .filter(|w| w.title.starts_with(&tag) && pids.contains(&w.process_id))
        .find(|w| {
            monitors
                .iter()
                .find(|m| m.id as i64 == w.monitor_id)
                .and_then(|m| native_sample(w, m, &monitors).ok().flatten())
                .is_some_and(|value| {
                    value["partial_clip"] == true && value["native_moving"] == false
                })
        })
        .context("no clipped owned Chrome fixture found")?;
    let hwnd = HWND(candidate.window_id as usize as *mut _);
    let mut live_pid = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut live_pid));
    }
    ensure!(
        live_pid == candidate.process_id && pids.contains(&live_pid),
        "fixture identity changed"
    );
    let monitor = monitors
        .iter()
        .find(|m| m.id as i64 == candidate.monitor_id)
        .unwrap();
    unsafe {
        ensure!(
            windows::Win32::Graphics::Gdi::SetWindowRgn(hwnd, None, true) != 0,
            "region removal failed"
        );
    }
    let started = Instant::now();
    loop {
        if let Some(value) = native_sample(&candidate, monitor, &monitors)? {
            if value["partial_clip"] == true && value["contained"] == true {
                let evidence = json!({"restored": true, "elapsed_ms": started.elapsed().as_millis(), "window": value});
                std::fs::write(report, serde_json::to_vec_pretty(&evidence)?)?;
                eprintln!(
                    "running daemon repaired fixture region after {} ms",
                    started.elapsed().as_millis()
                );
                return Ok(());
            }
        }
        ensure!(
            started.elapsed() < Duration::from_secs(3),
            "daemon did not repair stationary fixture within 3 seconds"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
