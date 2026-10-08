//! Opt-in real-app acceptance. Never uses an existing Chrome process/profile.

use super::*;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use windows::Win32::UI::WindowsAndMessaging::{SendMessageTimeoutW, SMTO_ABORTIFHUNG, WM_CLOSE};

struct BrowserFixture {
    child: Child,
    window_id: Option<WindowId>,
}

impl Drop for BrowserFixture {
    fn drop(&mut self) {
        if let Some(id) = self.window_id {
            let hwnd = window_id_to_hwnd(id).unwrap();
            let mut pid = 0;
            unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
            if pid == self.child.id() {
                let _ = restore_window_region(id);
                unsafe {
                    SendMessageTimeoutW(
                        hwnd,
                        WM_CLOSE,
                        windows::Win32::Foundation::WPARAM(0),
                        windows::Win32::Foundation::LPARAM(0),
                        SMTO_ABORTIFHUNG,
                        500,
                        None,
                    );
                }
            }
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "opt-in isolated Chrome; set LEOPARDWM_TEST_CLIP_CHROME_EXE to its exe"]
fn isolated_chrome_stays_contained_after_landing() {
    let Some(executable) = std::env::var_os("LEOPARDWM_TEST_CLIP_CHROME_EXE") else {
        return;
    };
    use windows::Win32::UI::HiDpi::{
        SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let profile = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("target")
        .join(format!(
            "clip-chrome-fixture-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
    std::fs::create_dir_all(&profile).unwrap();
    let child = Command::new(executable)
        .arg(format!("--user-data-dir={}", profile.display()))
        .args([
            "--no-first-run",
            "--no-default-browser-check",
            "--new-window",
            "about:blank",
            "--window-size=1000,700",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut fixture = BrowserFixture {
        child,
        window_id: None,
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    let id = loop {
        let owned: Vec<_> = crate::enumerate_windows()
            .unwrap()
            .into_iter()
            .filter(|window| {
                window.process_id == fixture.child.id() && window.class_name == "Chrome_WidgetWin_1"
            })
            .collect();
        assert!(
            owned.len() <= 1,
            "fixture must expose exactly one app window"
        );
        if let Some(window) = owned.first() {
            fixture.window_id = Some(window.hwnd);
            break window.hwnd;
        }
        assert!(Instant::now() < deadline, "isolated Chrome did not open");
        std::thread::sleep(Duration::from_millis(50));
    };
    std::thread::sleep(Duration::from_millis(300));
    let hwnd = window_id_to_hwnd(id).unwrap();
    let monitors = crate::enumerate_monitors().unwrap();
    let mut cache = crate::PlacementCache::new();
    // Changing presentation policy can leave the physical rectangle identical.
    // A cache hit must still acquire clipping ownership.
    let owner = monitors[0].rect;
    let cached_placement = leopardwm_core_layout::WindowPlacement {
        window_id: id,
        rect: Rect::new(owner.x + owner.width - 400, owner.y + 100, 1000, 700),
        visibility: leopardwm_core_layout::Visibility::Visible,
        column_index: 0,
    };
    let clipping_config = PlatformConfig {
        monitor_rects: monitors.iter().map(|m| m.rect).collect(),
        clip_owners: HashMap::from([(id, owner)]),
    };
    crate::apply_placements(
        std::slice::from_ref(&cached_placement),
        &PlatformConfig::default(),
        Some(&mut cache),
        false,
    )
    .unwrap();
    assert!(query_region(hwnd).unwrap().is_none());
    crate::apply_placements(
        &[cached_placement],
        &clipping_config,
        Some(&mut cache),
        false,
    )
    .unwrap();
    assert!(
        query_region(hwnd).unwrap().is_some(),
        "cache hit skipped acquiring the monitor clip"
    );
    restore_window_region(id).unwrap();
    for monitor in &monitors {
        let config = PlatformConfig {
            monitor_rects: monitors.iter().map(|m| m.rect).collect(),
            clip_owners: HashMap::from([(id, monitor.rect)]),
        };
        for (x, y) in [
            (monitor.rect.x + 100, monitor.rect.y + 100),
            (monitor.rect.x - 300, monitor.rect.y + 100),
            (
                monitor.rect.x + monitor.rect.width - 400,
                monitor.rect.y + 100,
            ),
            (monitor.rect.x + 100, monitor.rect.y - 200),
            (
                monitor.rect.x + 100,
                monitor.rect.y + monitor.rect.height - 300,
            ),
            (monitor.rect.x + 100, monitor.rect.y + 100),
        ] {
            let placement = leopardwm_core_layout::WindowPlacement {
                window_id: id,
                rect: Rect::new(x, y, 1000, 700),
                visibility: leopardwm_core_layout::Visibility::Visible,
                column_index: 0,
            };
            let owner = monitor.rect;
            crate::placement::observe_landing_flush(
                move || {
                    let outer = outer_rect(id).unwrap();
                    let bounds = local_clip(outer, owner, is_rtl(hwnd));
                    let region = query_region(hwnd).unwrap();
                    if full_clip(outer, bounds) {
                        assert!(
                            region.is_none(),
                            "Chrome presented a stale crop inside its monitor"
                        );
                    } else {
                        let expected = region_data(make_region(&[bounds]).unwrap().0).unwrap();
                        assert_eq!(region, expected, "Chrome presented an intermediate crop");
                    }
                },
                || {
                    crate::apply_placements(std::slice::from_ref(&placement), &config, None, false)
                        .unwrap();
                },
            );
            crate::apply_placements(
                std::slice::from_ref(&placement),
                &config,
                Some(&mut cache),
                false,
            )
            .unwrap();
            crate::apply_placements(std::slice::from_ref(&placement), &config, None, false)
                .unwrap();
            unsafe { windows::Win32::Graphics::Dwm::DwmFlush().unwrap() };
            // Give Chromium's own frame/region updates time to run after landing.
            std::thread::sleep(Duration::from_millis(200));
            let outer = outer_rect(id).unwrap();
            let bounds = local_clip(outer, monitor.rect, is_rtl(hwnd));
            let region = query_region(hwnd).unwrap();
            eprintln!(
                "chrome-clip monitor={} dpi={} outer={outer:?} region={region:?} clip={bounds:?}",
                monitor.device_name, monitor.scale_factor
            );
            if !full_clip(outer, bounds) {
                let rects = region.expect("Chromium removed the monitor region after landing");
                assert!(!rects.is_empty(), "visible slice disappeared");
                assert!(
                    rects.iter().all(|r| {
                        r[0] >= bounds[0]
                            && r[1] >= bounds[1]
                            && r[2] <= bounds[2]
                            && r[3] <= bounds[3]
                    }),
                    "Chromium region extends onto another monitor"
                );
                assert!(!monitor_clip_repair_needed(&config).unwrap());
                // Model an application replacing its own region after landing,
                // with unchanged geometry and an already populated frame cache.
                install_region(hwnd, &None).unwrap();
                assert!(monitor_clip_repair_needed(&config).unwrap());
                crate::apply_placements(&[placement], &config, Some(&mut cache), false).unwrap();
                assert!(!monitor_clip_repair_needed(&config).unwrap());
            }
        }
        restore_window_region(id).unwrap();
    }
}
