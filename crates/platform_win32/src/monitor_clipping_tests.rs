use super::*;

#[test]
fn clips_both_edges_at_negative_monitor_origins() {
    let monitor = Rect::new(-2560, -100, 2560, 1440);
    assert_eq!(
        local_clip(Rect::new(-2800, 30, 800, 600), monitor, false),
        [240, 0, 800, 600]
    );
    assert_eq!(
        local_clip(Rect::new(-400, 30, 800, 600), monitor, false),
        [0, 0, 400, 600]
    );
    assert_eq!(
        local_clip(Rect::new(-400, 30, 800, 600), monitor, true),
        [400, 0, 800, 600]
    );
}

#[test]
fn clips_vertical_overflow_and_nonadjacent_monitors() {
    let owner = Rect::new(0, 0, 1080, 1920);
    assert_eq!(
        local_clip(Rect::new(100, -200, 1400, 2400), owner, false),
        [0, 200, 980, 2120]
    );
    assert_eq!(
        local_clip(Rect::new(1500, 100, 800, 500), owner, false),
        [0, 0, 0, 500]
    );
}

#[test]
fn absent_empty_and_complex_regions_are_distinct_and_reversible() {
    let clip = [100, 0, 500, 600];
    assert_eq!(apply_baseline(&None, clip), Some(vec![clip]));
    assert_eq!(apply_baseline(&Some(vec![]), clip), Some(vec![]));
    let original = Some(vec![[0, 0, 200, 100], [300, 400, 700, 600]]);
    assert_eq!(
        apply_baseline(&original, clip),
        Some(vec![[100, 0, 200, 100], [300, 400, 500, 600]])
    );
    // Each scroll derives from the baseline, never the previous truncated slice.
    assert_eq!(apply_baseline(&original, [0, 0, 800, 600]), original);
}

#[test]
fn intermediate_clip_is_safe_at_old_and_new_positions() {
    let monitor = Rect::new(0, 0, 1920, 1080);
    for rtl in [false, true] {
        for x in [-800, -200, 0, 1500, 1920] {
            for next in [-800, -200, 0, 1500, 1920] {
                let a = local_clip(Rect::new(x, 0, 800, 600), monitor, rtl);
                let b = local_clip(Rect::new(next, 0, 800, 600), monitor, rtl);
                let tight = intersect(a, b);
                for bound in [a, b] {
                    if tight[0] < tight[2] && tight[1] < tight[3] {
                        assert!(tight[0] >= bound[0] && tight[2] <= bound[2]);
                        assert!(tight[1] >= bound[1] && tight[3] <= bound[3]);
                    }
                }
            }
        }
    }
}

#[test]
fn huge_desktop_edges_do_not_overflow() {
    let outer = Rect::new(i32::MAX - 500, i32::MIN, 1000, 800);
    assert_eq!(
        local_clip(
            outer,
            Rect::new(i32::MAX - 300, i32::MIN + 50, 1000, 1000),
            false
        ),
        [200, 50, 1000, 800]
    );
}

#[test]
fn rtl_resize_moves_existing_regions_without_changing_their_shape() {
    let region = Some(vec![[0, 0, 514, 507]]);
    assert_eq!(
        region_at_width(&region, 814, 2576, true),
        Some(vec![[1762, 0, 2276, 507]])
    );
    assert_eq!(region_at_width(&region, 814, 2576, false), region);
    assert_eq!(region_at_width(&None, 814, 2576, true), None);
    assert_eq!(
        region_at_width(&Some(vec![]), 814, 2576, true),
        Some(vec![])
    );
}

#[test]
fn native_region_roundtrip_preserves_complex_and_empty_shapes() {
    for source in [
        vec![],
        vec![[0, 0, 10, 20]],
        vec![[0, 0, 10, 20], [40, 50, 100, 120]],
    ] {
        let region = make_region(&source).unwrap();
        assert_eq!(region_data(region.0).unwrap(), Some(source));
    }
}

#[test]
fn containment_distinguishes_missing_regions_from_empty_and_bounded_regions() {
    let bounds = [100, 0, 500, 600];
    assert!(!region_is_contained(&None, bounds));
    assert!(region_is_contained(&Some(vec![]), bounds));
    assert!(region_is_contained(
        &Some(vec![[100, 0, 200, 100], [300, 400, 500, 600]]),
        bounds
    ));
    for r in [
        [99, 0, 200, 100],
        [100, -1, 200, 100],
        [100, 0, 501, 600],
        [100, 0, 500, 601],
    ] {
        assert!(!region_is_contained(&Some(vec![r]), bounds));
    }
}

// Opt-in test uses production placement and recovery, in a deadline-supervised
// child. It creates only owned non-activating, tool-window fixtures; it never
// selects, closes or manipulates an existing user application.
const NATIVE_TEST: &str = "monitor_clipping::tests::native_scrolling_and_journal_recovery";

#[test]
#[ignore = "opt-in native desktop fixture; set LEOPARDWM_TEST_MONITOR_CLIPPING=1"]
fn native_scrolling_and_journal_recovery() {
    if std::env::var("LEOPARDWM_TEST_MONITOR_CLIPPING").as_deref() != Ok("1") {
        return;
    }
    if std::env::var("LEOPARDWM_TEST_MONITOR_CLIPPING_RECOVER").as_deref() == Ok("1") {
        restore_all_window_regions().unwrap();
        return;
    }
    if std::env::var("LEOPARDWM_TEST_MONITOR_CLIPPING_CHILD").as_deref() == Ok("1") {
        native_fixture();
        return;
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", NATIVE_TEST, "--ignored", "--nocapture"])
        .env("LEOPARDWM_TEST_MONITOR_CLIPPING_CHILD", "1")
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("native clipping fixture exceeded deadline");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

struct Fixture(HWND);
impl Drop for Fixture {
    fn drop(&mut self) {
        let id = self.0 .0 as usize as u64;
        let _ = restore_window_region(id);
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(self.0);
        }
    }
}

fn native_fixture() {
    use windows::Win32::UI::HiDpi::{
        SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    use windows::Win32::UI::WindowsAndMessaging::*;
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let monitors = crate::enumerate_monitors().unwrap();
    assert!(
        monitors.len() >= 2,
        "this acceptance test requires two actual monitors"
    );
    for (monitor, rtl) in monitors
        .iter()
        .flat_map(|monitor| [false, true].map(|rtl| (monitor, rtl)))
    {
        eprintln!(
            "native-monitor-clip owner={} rect={:?} dpi={} rtl={rtl}",
            monitor.device_name, monitor.rect, monitor.scale_factor
        );
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE
                    | if rtl {
                        WS_EX_LAYOUTRTL
                    } else {
                        WINDOW_EX_STYLE(0)
                    },
                w!("STATIC"),
                w!("LeopardWM monitor clipping fixture"),
                WS_OVERLAPPEDWINDOW,
                monitor.rect.x + 100,
                monitor.rect.y + 100,
                800,
                500,
                None,
                None,
                None,
                None,
            )
            .unwrap()
        };
        let fixture = Fixture(hwnd);
        let id = hwnd.0 as usize as u64;
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            windows::Win32::Graphics::Dwm::DwmFlush().unwrap();
        }
        let owner = monitor.rect;
        let config = PlatformConfig {
            monitor_rects: monitors.iter().map(|m| m.rect).collect(),
            clip_owners: HashMap::from([(id, owner)]),
        };
        let insets = crate::get_window_invisible_insets(id);
        exercise_landing_presentation(id, owner, &config);
        let mut client_before = RECT::default();
        let mut cache = crate::PlacementCache::new();
        for x in [
            owner.x + 100,
            owner.x - 400,
            owner.x + owner.width - 400,
            owner.x - 50,
            owner.x + 100,
        ] {
            let placement = leopardwm_core_layout::WindowPlacement {
                window_id: id,
                rect: Rect::new(x, owner.y + 100, 800, 500),
                visibility: leopardwm_core_layout::Visibility::Visible,
                column_index: 0,
            };
            crate::apply_placements(
                std::slice::from_ref(&placement),
                &config,
                Some(&mut cache),
                false,
            )
            .unwrap();
            let result = crate::apply_placements(&[placement], &config, None, false).unwrap();
            eprintln!(
                "native-monitor-clip x={x} insets={insets:?} visible={:?} outer={:?}",
                crate::get_window_visible_rect(id),
                outer_rect(id)
            );
            assert!(
                result.width_violations.is_empty() && result.height_violations.is_empty(),
                "clipping must not create size feedback"
            );
            assert_eq!(
                crate::get_window_visible_rect(id).unwrap(),
                Rect::new(x, owner.y + 100, 800, 500)
            );
            let outer = outer_rect(id).unwrap();
            assert_eq!(outer.width, 800 + insets.0 + insets.2);
            let mut client = RECT::default();
            unsafe {
                GetClientRect(hwnd, &mut client).unwrap();
            }
            if client_before.right != 0 {
                assert_eq!(
                    client, client_before,
                    "scrolling changed the client dimensions"
                );
            }
            client_before = client;
            let clip = local_clip(outer, owner, rtl);
            let region = query_region(hwnd).unwrap();
            if full_clip(outer, clip) {
                assert!(
                    region.is_none(),
                    "fully visible window must regain its absent region"
                );
            } else {
                assert_eq!(
                    region,
                    region_data(make_region(&[clip]).unwrap().0).unwrap()
                );
            }
        }
        // Complex baseline survives clipping and a recovery with lost process-local
        // cache: exactly the information a separate watchdog would have available.
        let original = Some(vec![[0, 0, 200, 200], [400, 100, 600, 400]]);
        install_region(hwnd, &original).unwrap();
        let canonical = query_region(hwnd).unwrap();
        let target = Rect::new(
            owner.x - 300,
            owner.y + 100,
            800 + insets.0 + insets.2,
            500 + insets.1 + insets.3,
        );
        prepare_clips(
            &HashMap::from([(id, (target, insets))]),
            &config,
            &std::collections::HashSet::new(),
        )
        .unwrap();
        assert!(read_journal(id).unwrap().is_some());
        recover_in_separate_process();
        CLIPS.lock().unwrap().remove(&id);
        assert_eq!(query_region(hwnd).unwrap(), canonical);
        assert!(read_journal(id).unwrap().is_none());
        exercise_release_and_pending_regions(id, hwnd, target, insets, &config);
        // A stale token must never restore a replacement lifetime's region.
        install_region(hwnd, &None).unwrap();
        prepare_clips(
            &HashMap::from([(id, (target, insets))]),
            &config,
            &std::collections::HashSet::new(),
        )
        .unwrap();
        let app_region = Some(vec![[20, 20, 50, 50]]);
        install_region(hwnd, &app_region).unwrap();
        unsafe {
            SetPropW(hwnd, PROPERTY, Some(HANDLE(42_usize as *mut c_void))).unwrap();
        }
        restore_window_region(id).unwrap();
        assert_eq!(query_region(hwnd).unwrap(), app_region);
        unsafe {
            RemovePropW(hwnd, PROPERTY).unwrap();
        }
        drop(fixture);
    }
}

fn exercise_landing_presentation(id: WindowId, owner: Rect, config: &PlatformConfig) {
    let observations = std::rc::Rc::new(std::cell::Cell::new(0));
    let observed = observations.clone();
    crate::placement::observe_landing_flush(
        move || {
            let hwnd = window_id_to_hwnd(id).unwrap();
            let outer = outer_rect(id).unwrap();
            let bounds = local_clip(outer, owner, is_rtl(hwnd));
            let region = query_region(hwnd).unwrap();
            let expected = if full_clip(outer, bounds) {
                None
            } else {
                region_data(make_region(&[bounds]).unwrap().0).unwrap()
            };
            assert_eq!(
                region, expected,
                "composition barrier presented the intermediate crop at {outer:?}"
            );
            observed.set(observed.get() + 1);
        },
        || {
            // Direct synchronous focus jumps must be correct at presentation,
            // including changes between clipped and fully visible endpoints.
            for x in [
                owner.x + 100,
                owner.x - 400,
                owner.x + 100,
                owner.x + owner.width - 400,
                owner.x + 100,
            ] {
                let placement = leopardwm_core_layout::WindowPlacement {
                    window_id: id,
                    rect: Rect::new(x, owner.y + 100, 800, 500),
                    visibility: leopardwm_core_layout::Visibility::Visible,
                    column_index: 0,
                };
                crate::apply_placements(&[placement], config, None, false).unwrap();
            }
        },
    );
    assert!(
        observations.get() >= 5,
        "fixture did not reach presentation"
    );
}

fn recover_in_separate_process() {
    use windows::Win32::UI::WindowsAndMessaging::*;
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", NATIVE_TEST, "--ignored", "--nocapture"])
        .env("LEOPARDWM_TEST_MONITOR_CLIPPING_RECOVER", "1")
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        // Dispatch sent messages on the fixture's owning thread while the other
        // process calls SetWindowRgn. This is a fixture pump, not app automation.
        let mut message = MSG::default();
        unsafe {
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                DispatchMessageW(&message);
            }
        }
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("cross-process region recovery timed out");
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn exercise_release_and_pending_regions(
    id: WindowId,
    hwnd: HWND,
    target: Rect,
    insets: Insets,
    config: &PlatformConfig,
) {
    use std::collections::HashSet;
    let targets = HashMap::from([(id, (target, insets))]);
    for baseline in [None, Some(vec![]), Some(vec![[10, 10, 600, 400]])] {
        install_region(hwnd, &baseline).unwrap();
        prepare_clips(&targets, config, &HashSet::new()).unwrap();
        let before = query_region(hwnd).unwrap();
        // A pending move must never relax its restrictive region early.
        finish_clips(config, &HashSet::from([id])).unwrap();
        assert_eq!(query_region(hwnd).unwrap(), before);
        // Floating/fullscreen/unmanaged configuration releases the native clip.
        finish_clips(&PlatformConfig::default(), &HashSet::new()).unwrap();
        assert_eq!(query_region(hwnd).unwrap(), baseline);
        assert!(read_journal(id).unwrap().is_none());
    }
    install_region(hwnd, &None).unwrap();
    // Even if the daemon's owner snapshot has not yet caught up with a native
    // maximize, the platform releases the clip as soon as it observes IsZoomed.
    prepare_clips(&targets, config, &HashSet::new()).unwrap();
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(
            hwnd,
            windows::Win32::UI::WindowsAndMessaging::SW_MAXIMIZE,
        );
    }
    assert!(crate::is_window_maximized(id));
    finish_clips(config, &HashSet::new()).unwrap();
    assert!(query_region(hwnd).unwrap().is_none());
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(
            hwnd,
            windows::Win32::UI::WindowsAndMessaging::SW_RESTORE,
        );
    }
}
