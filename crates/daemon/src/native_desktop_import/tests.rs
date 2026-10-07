use super::*;
use leopardwm_core_layout::Rect;
use transaction::{Backend, Paths};

fn inventory() -> native::Inventory {
    let desktops = ["Personal", "Growth", "WellBet"]
        .into_iter()
        .enumerate()
        .map(|(i, name)| native::Desktop {
            id: format!("D{i}"),
            name: name.into(),
        })
        .collect();
    let windows = (0..6)
        .map(|i| native::Window {
            hwnd: 100 + i,
            pid: 10,
            tid: 20,
            desktop: format!("D{}", i / 2),
            pinned: false,
            title: format!("Window {i}"),
            class_name: "TestApp".into(),
            executable: "app.exe".into(),
            monitor: format!("DISPLAY{}", i % 2 + 1),
            rect: Rect::new(20, 20, 640, 480),
            minimized: i == 4,
            manageable: true,
            placement: native::Placement {
                flags: 0,
                show_cmd: 1,
                min: [0, 0],
                max: [0, 0],
                normal: [20, 20, 660, 500],
            },
        })
        .collect();
    native::Inventory {
        desktops,
        current: "D1".into(),
        windows,
    }
}

fn monitors() -> Vec<MonitorInfo> {
    (1..=2)
        .map(|i| MonitorInfo {
            id: i,
            rect: Rect::new((i as i32 - 1) * 1920, 0, 1920, 1080),
            work_area: Rect::new((i as i32 - 1) * 1920, 0, 1920, 1040),
            is_primary: i == 1,
            device_name: format!("DISPLAY{i}"),
            scale_factor: 1.0,
        })
        .collect()
}

#[test]
fn preserves_desktop_names_monitor_membership_and_current_workspace() {
    let p = make_plan(&Config::default(), inventory(), monitors()).unwrap();
    let config: Config = toml::from_str(&p.config_after).unwrap();
    assert_eq!(config.workspaces.names, ["Personal", "Growth", "WellBet"]);
    let snapshot: crate::state::StateSnapshot = serde_json::from_str(&p.state_after).unwrap();
    assert_eq!(snapshot.active_workspace["DISPLAY1"], 1);
    assert_eq!(snapshot.active_workspace["DISPLAY2"], 1);
    for e in &p.entries {
        let ws = snapshot
            .workspaces
            .iter()
            .find(|w| w.monitor_device_name == e.window.monitor && w.workspace_index == e.workspace)
            .unwrap();
        assert!(ws.workspace.contains_window(e.window.hwnd));
    }
    assert_eq!(p.entries.len(), 6);
}

#[test]
fn rejects_too_many_desktops_and_duplicate_handles() {
    let mut inv = inventory();
    inv.desktops.extend((3..10).map(|i| native::Desktop {
        id: format!("D{i}"),
        name: i.to_string(),
    }));
    assert!(make_plan(&Config::default(), inv, monitors()).is_err());
    let mut inv = inventory();
    inv.windows.push(inv.windows[0].clone());
    assert!(make_plan(&Config::default(), inv, monitors()).is_err());
}

#[test]
fn leaves_pinned_ignored_and_unmanageable_windows_out_of_layout() {
    let mut inv = inventory();
    inv.windows[0].pinned = true;
    inv.windows[1].manageable = false;
    let config: Config =
        toml::from_str("[[window_rules]]\nmatch_title='Window 2'\naction='ignore'").unwrap();
    let p = make_plan(&config, inv, monitors()).unwrap();
    assert_eq!(p.entries.len(), 3);
    assert_eq!(p.skipped.len(), 3);
}

struct Fake {
    inventory: native::Inventory,
    stamps: HashMap<u64, u64>,
    fail_after_move: bool,
    normalized: Vec<u64>,
    removed: Vec<String>,
}
impl Fake {
    fn new() -> Self {
        Self {
            inventory: inventory(),
            stamps: HashMap::new(),
            fail_after_move: false,
            normalized: Vec::new(),
            removed: Vec::new(),
        }
    }
}
impl Backend for Fake {
    fn inventory(&mut self) -> Result<native::Inventory> {
        Ok(self.inventory.clone())
    }
    fn stamp(&mut self, w: &native::Window, t: u64) -> Result<()> {
        self.stamps.insert(w.hwnd, t);
        Ok(())
    }
    fn same_lifetime(&mut self, w: &native::Window, t: u64) -> bool {
        self.stamps.get(&w.hwnd) == Some(&t)
    }
    fn desktop(&mut self, w: &native::Window) -> Result<String> {
        Ok(self
            .inventory
            .windows
            .iter()
            .find(|v| v.hwnd == w.hwnd)
            .unwrap()
            .desktop
            .clone())
    }
    fn move_window(
        &mut self,
        w: &native::Window,
        t: u64,
        source: &str,
        target: &str,
    ) -> Result<()> {
        ensure!(self.same_lifetime(w, t), "stale window");
        let window = self
            .inventory
            .windows
            .iter_mut()
            .find(|v| v.hwnd == w.hwnd)
            .unwrap();
        ensure!(window.desktop == source, "wrong desktop");
        window.desktop = target.into();
        if std::mem::take(&mut self.fail_after_move) {
            anyhow::bail!("failure after native mutation");
        }
        Ok(())
    }
    fn normalize(&mut self, w: &native::Window, _: u64) -> Result<()> {
        self.normalized.push(w.hwnd);
        Ok(())
    }
    fn restore_placement(&mut self, _: &native::Window, _: u64) -> Result<()> {
        Ok(())
    }
    fn remove_empty(&mut self, id: &str, _: &str) -> Result<bool> {
        if self
            .inventory
            .windows
            .iter()
            .any(|w| w.desktop == id && !w.pinned)
        {
            return Ok(false);
        }
        self.removed.push(id.into());
        self.inventory.desktops.retain(|d| d.id != id);
        Ok(true)
    }
    fn create(&mut self, name: &str) -> Result<String> {
        let id = format!("recreated-{}", self.inventory.desktops.len());
        self.inventory.desktops.push(native::Desktop {
            id: id.clone(),
            name: name.into(),
        });
        Ok(id)
    }
}

struct Files {
    root: PathBuf,
    paths: Paths,
}
impl Files {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "leopardwm-import-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let paths = Paths {
            config: root.join("config.toml"),
            state: root.join("state.json"),
            backups: root.join("backups"),
        };
        std::fs::write(&paths.config, b"original config").unwrap();
        std::fs::write(&paths.state, b"original state").unwrap();
        Self { root, paths }
    }
}
impl Drop for Files {
    fn drop(&mut self) {
        if let (Ok(root), Ok(temp)) = (
            self.root.canonicalize(),
            std::env::temp_dir().canonicalize(),
        ) {
            if root.parent() == Some(temp.as_path())
                && root
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("leopardwm-import-test-")
            {
                let _ = std::fs::remove_dir_all(root);
            }
        }
    }
}

#[test]
fn rejects_file_changes_since_planning_without_moving_windows() {
    let files = Files::new();
    let original = files.paths.snapshot().unwrap();
    let mut backend = Fake::new();
    let p = make_plan(&Config::default(), backend.inventory.clone(), monitors()).unwrap();
    std::fs::write(&files.paths.config, b"concurrent user edit").unwrap();
    let error = transaction::apply(p, &files.paths, &mut backend, false, original).unwrap_err();
    assert!(error.to_string().contains("changed while planning"));
    assert!(backend.stamps.is_empty());
    assert!(backend.removed.is_empty());
    assert_eq!(
        std::fs::read(&files.paths.config).unwrap(),
        b"concurrent user edit"
    );
    assert_eq!(
        std::fs::read(&files.paths.state).unwrap(),
        b"original state"
    );
}

#[test]
fn failed_move_rolls_back_even_if_native_api_mutated_before_returning_error() {
    let files = Files::new();
    let mut backend = Fake::new();
    backend.fail_after_move = true;
    let p = make_plan(&Config::default(), backend.inventory.clone(), monitors()).unwrap();
    assert!(transaction::apply(
        p,
        &files.paths,
        &mut backend,
        false,
        files.paths.snapshot().unwrap()
    )
    .is_err());
    assert!(backend.removed.is_empty());
    assert_eq!(
        std::fs::read(&files.paths.config).unwrap(),
        b"original config"
    );
    assert_eq!(
        std::fs::read(&files.paths.state).unwrap(),
        b"original state"
    );
    for (a, b) in backend.inventory.windows.iter().zip(inventory().windows) {
        assert_eq!(a.desktop, b.desktop);
    }
}

#[test]
fn commits_layout_before_removing_desktops_and_can_restore_deleted_desktops() {
    let files = Files::new();
    let mut backend = Fake::new();
    let p = make_plan(&Config::default(), backend.inventory.clone(), monitors()).unwrap();
    let dir = transaction::apply(
        p,
        &files.paths,
        &mut backend,
        false,
        files.paths.snapshot().unwrap(),
    )
    .unwrap();
    assert_eq!(backend.inventory.desktops.len(), 1);
    assert!(backend.inventory.windows.iter().all(|w| w.desktop == "D1"));
    assert!(
        !backend.normalized.contains(&104),
        "minimized window must stay minimized"
    );
    let state: crate::state::StateSnapshot =
        serde_json::from_slice(&std::fs::read(&files.paths.state).unwrap()).unwrap();
    assert_eq!(state.workspaces.len(), 6);
    transaction::restore(&dir, &files.paths, &mut backend).unwrap();
    assert_eq!(backend.inventory.desktops.len(), 3);
    assert_eq!(
        std::fs::read(&files.paths.config).unwrap(),
        b"original config"
    );
    assert_eq!(
        std::fs::read(&files.paths.state).unwrap(),
        b"original state"
    );
    for window in &backend.inventory.windows {
        let name = &backend
            .inventory
            .desktops
            .iter()
            .find(|d| d.id == window.desktop)
            .unwrap()
            .name;
        assert_eq!(
            name,
            ["Personal", "Growth", "WellBet"][(window.hwnd as usize - 100) / 2]
        );
    }
}

#[test]
fn keeps_nonempty_native_desktops_and_skips_recycled_hwnds_during_restore() {
    let files = Files::new();
    let mut backend = Fake::new();
    backend.inventory.windows[0].manageable = false;
    let p = make_plan(&Config::default(), backend.inventory.clone(), monitors()).unwrap();
    let dir = transaction::apply(
        p,
        &files.paths,
        &mut backend,
        false,
        files.paths.snapshot().unwrap(),
    )
    .unwrap();
    assert!(backend.inventory.desktops.iter().any(|d| d.id == "D0"));
    backend.stamps.remove(&105);
    transaction::restore(&dir, &files.paths, &mut backend).unwrap();
    assert_eq!(
        backend
            .inventory
            .windows
            .iter()
            .find(|w| w.hwnd == 105)
            .unwrap()
            .desktop,
        "D1"
    );
}

#[test]
fn repeated_import_cannot_flatten_workspaces_when_an_excluded_window_keeps_a_native_desktop() {
    let files = Files::new();
    let mut backend = Fake::new();
    backend.inventory.windows[0].manageable = false;
    let p = make_plan(&Config::default(), backend.inventory.clone(), monitors()).unwrap();
    transaction::apply(
        p,
        &files.paths,
        &mut backend,
        false,
        files.paths.snapshot().unwrap(),
    )
    .unwrap();
    let saved = std::fs::read(&files.paths.state).unwrap();
    let repeated = make_plan(&Config::default(), backend.inventory.clone(), monitors()).unwrap();
    assert!(transaction::apply(
        repeated,
        &files.paths,
        &mut backend,
        false,
        files.paths.snapshot().unwrap()
    )
    .is_err());
    assert_eq!(std::fs::read(&files.paths.state).unwrap(), saved);
}

#[test]
fn keep_native_option_preserves_empty_desktops_and_unfinished_journal_requires_recovery() {
    let files = Files::new();
    let mut backend = Fake::new();
    let p = make_plan(&Config::default(), backend.inventory.clone(), monitors()).unwrap();
    let dir = transaction::apply(
        p,
        &files.paths,
        &mut backend,
        true,
        files.paths.snapshot().unwrap(),
    )
    .unwrap();
    assert_eq!(backend.inventory.desktops.len(), 3);
    assert!(backend.removed.is_empty());
    let journal = dir.join("journal.json");
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&journal).unwrap()).unwrap();
    json["phase"] = serde_json::json!("moving");
    std::fs::write(&journal, serde_json::to_vec(&json).unwrap()).unwrap();
    let p = make_plan(&Config::default(), backend.inventory.clone(), monitors()).unwrap();
    assert!(transaction::apply(
        p,
        &files.paths,
        &mut backend,
        true,
        files.paths.snapshot().unwrap()
    )
    .is_err());
    transaction::restore(&dir, &files.paths, &mut backend).unwrap();
    assert_eq!(
        std::fs::read(&files.paths.state).unwrap(),
        b"original state"
    );
}

#[test]
fn import_flags_require_explicit_mutation_and_reject_conflicting_modes() {
    use clap::Parser;
    let args = crate::Args::try_parse_from(["leopardwm", "import-native-desktops"]).unwrap();
    assert!(matches!(
        args.offline,
        Some(OfflineCommand::ImportNativeDesktops { apply: false, .. })
    ));
    assert!(crate::Args::try_parse_from([
        "leopardwm",
        "import-native-desktops",
        "--keep-native-desktops"
    ])
    .is_err());
    assert!(crate::Args::try_parse_from([
        "leopardwm",
        "import-native-desktops",
        "--apply",
        "--restore",
        "backup"
    ])
    .is_err());
}

// These ignored fixtures are intentionally isolated from existing applications
// and desktops. All mutations are guarded by exact fixture HWNDs/PID and the
// set of desktop IDs created by this test.
struct ScopedNative {
    backend: transaction::NativeBackend,
    current: String,
    owned_desktops: HashSet<String>,
    ids: HashSet<u64>,
    pid: u32,
    child: Option<std::process::Child>,
}
impl ScopedNative {
    fn owns(&self, w: &native::Window) -> Result<()> {
        ensure!(
            w.pid == self.pid && self.ids.contains(&w.hwnd),
            "Test refuses to mutate a non-fixture window"
        );
        Ok(())
    }
    fn cleanup(&mut self) -> Result<()> {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        for id in self.owned_desktops.clone() {
            if native::desktops()?.iter().any(|d| d.id == id) {
                ensure!(
                    native::remove_empty_desktop(&id, &self.current)?,
                    "Owned test desktop remained nonempty"
                );
            }
        }
        Ok(())
    }
}
impl Drop for ScopedNative {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            eprintln!("Native fixture cleanup failed: {error:#}");
        }
    }
}
impl Backend for ScopedNative {
    fn inventory(&mut self) -> Result<native::Inventory> {
        let mut inv = self.backend.inventory()?;
        ensure!(
            inv.current == self.current,
            "User switched native desktops during fixture"
        );
        inv.desktops
            .retain(|d| d.id == self.current || self.owned_desktops.contains(&d.id));
        inv.windows
            .retain(|w| w.pid == self.pid && self.ids.contains(&w.hwnd));
        Ok(inv)
    }
    fn stamp(&mut self, w: &native::Window, t: u64) -> Result<()> {
        self.owns(w)?;
        self.backend.stamp(w, t)
    }
    fn same_lifetime(&mut self, w: &native::Window, t: u64) -> bool {
        self.owns(w).is_ok() && self.backend.same_lifetime(w, t)
    }
    fn desktop(&mut self, w: &native::Window) -> Result<String> {
        self.owns(w)?;
        self.backend.desktop(w)
    }
    fn move_window(
        &mut self,
        w: &native::Window,
        t: u64,
        source: &str,
        target: &str,
    ) -> Result<()> {
        self.owns(w)?;
        ensure!(
            target == self.current || self.owned_desktops.contains(target),
            "Non-fixture destination"
        );
        self.backend.move_window(w, t, source, target)
    }
    fn normalize(&mut self, w: &native::Window, t: u64) -> Result<()> {
        self.owns(w)?;
        self.backend.normalize(w, t)
    }
    fn restore_placement(&mut self, w: &native::Window, t: u64) -> Result<()> {
        self.owns(w)?;
        self.backend.restore_placement(w, t)
    }
    fn remove_empty(&mut self, id: &str, fallback: &str) -> Result<bool> {
        ensure!(
            self.owned_desktops.contains(id) && fallback == self.current,
            "Test refuses to remove a user desktop"
        );
        self.backend.remove_empty(id, fallback)
    }
    fn create(&mut self, name: &str) -> Result<String> {
        ensure!(
            name.starts_with("LeopardWM import fixture "),
            "Test refuses to recreate a user desktop"
        );
        let id = self.backend.create(name)?;
        self.owned_desktops.insert(id.clone());
        Ok(id)
    }
}

#[test]
#[ignore = "disposable desktops + windows; requires LEOPARDWM_TEST_NATIVE_IMPORT=1"]
fn native_migration_moves_other_process_windows_and_restores_grouping() -> Result<()> {
    use std::time::{Duration, Instant};
    ensure!(
        std::env::var("LEOPARDWM_TEST_NATIVE_IMPORT").as_deref() == Ok("1"),
        "Explicit native test opt-in required"
    );
    leopardwm_platform_win32::set_dpi_awareness();
    let files = Files::new();
    let mut scope = ScopedNative {
        backend: transaction::NativeBackend,
        current: native::current_desktop()?,
        owned_desktops: HashSet::new(),
        ids: HashSet::new(),
        pid: 0,
        child: None,
    };
    let original_desktops = native::desktops()?;
    let source =
        native::create_desktop(&format!("LeopardWM import fixture {}", std::process::id()))?;
    scope.owned_desktops.insert(source.clone());
    let child = std::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "native_desktop_import::tests::native_import_child",
            "--ignored",
            "--nocapture",
        ])
        .env("LEOPARDWM_NATIVE_IMPORT_FIXTURE_DIR", &files.root)
        .stdout(std::process::Stdio::null())
        .stderr(std::fs::File::create(files.root.join("child.stderr"))?)
        .spawn()?;
    scope.pid = child.id();
    scope.child = Some(child);
    let ready = files.root.join("ready.json");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() {
        ensure!(Instant::now() < deadline, "Fixture not ready");
        std::thread::sleep(Duration::from_millis(25));
    }
    let ids: Vec<u64> = serde_json::from_slice(&std::fs::read(ready)?)?;
    ensure!(
        ids.len() == 3,
        "Expected exactly three owned fixture windows"
    );
    scope.ids.extend(&ids);
    // Wait for shell app-view registration, then move two child-owned HWNDs.
    let mut inv;
    loop {
        inv = scope.inventory()?;
        if inv.windows.len() == 3 {
            break;
        }
        ensure!(Instant::now() < deadline, "Fixture registration not ready");
        std::thread::sleep(Duration::from_millis(25));
    }
    for id in &ids[1..] {
        let window = inv.windows.iter().find(|w| w.hwnd == *id).unwrap();
        scope.stamp(window, 1)?;
        let current = scope.current.clone();
        scope.move_window(window, 1, &current, &source)?;
    }
    let inventory = scope.inventory()?;
    let original: HashMap<_, _> = inventory
        .windows
        .iter()
        .map(|w| (w.hwnd, w.desktop.clone()))
        .collect();
    let plan = make_plan(
        &Config::default(),
        inventory,
        leopardwm_platform_win32::enumerate_monitors()?,
    )?;
    ensure!(plan.entries.len() == 3, "Fixture was unexpectedly excluded");
    let imported = plan.entries.clone();
    let backup = transaction::apply(
        plan,
        &files.paths,
        &mut scope,
        false,
        files.paths.snapshot()?,
    )?;
    ensure!(
        !native::desktops()?.iter().any(|d| d.id == source),
        "Imported empty desktop was not removed"
    );
    for entry in &imported {
        ensure!(
            native::window_desktop(entry.window.hwnd)? == scope.current,
            "Window did not migrate"
        );
    }
    ensure!(
        !leopardwm_platform_win32::is_window_maximized(ids[1]),
        "Tiled maximized window was not normalized"
    );
    ensure!(
        leopardwm_platform_win32::window_minimized_state(ids[2]) == Some(true),
        "Minimized state was lost"
    );
    let snapshot: crate::state::StateSnapshot =
        serde_json::from_slice(&std::fs::read(&files.paths.state)?)?;
    for entry in &imported {
        ensure!(
            snapshot
                .workspaces
                .iter()
                .any(|ws| ws.monitor_device_name == entry.window.monitor
                    && ws.workspace_index == entry.workspace
                    && ws.workspace.contains_window(entry.window.hwnd)),
            "Wrong saved workspace/monitor"
        );
    }
    transaction::restore(&backup, &files.paths, &mut scope)?;
    let restored = scope.inventory()?;
    for window in &restored.windows {
        if original[&window.hwnd] == scope.current {
            ensure!(
                window.desktop == scope.current,
                "Local fixture moved during restore"
            );
        } else {
            ensure!(
                window.desktop != scope.current && window.desktop != source,
                "Missing recreated native desktop"
            );
        }
    }
    ensure!(
        std::fs::read(&files.paths.config)? == b"original config",
        "Config backup changed"
    );
    ensure!(
        std::fs::read(&files.paths.state)? == b"original state",
        "State backup changed"
    );
    scope.cleanup()?;
    ensure!(
        native::desktops()? == original_desktops,
        "User native desktop list changed during fixture"
    );
    println!("PASS: cross-process import, names/monitors, minimized/maximized handling, empty desktop removal and recovery; original native desktops preserved");
    Ok(())
}

#[test]
#[ignore = "child process for the native migration fixture only"]
fn native_import_child() -> Result<()> {
    use windows::core::w;
    use windows::Win32::UI::WindowsAndMessaging::*;
    let root = PathBuf::from(
        std::env::var_os("LEOPARDWM_NATIVE_IMPORT_FIXTURE_DIR").context("Fixture host required")?,
    );
    leopardwm_platform_win32::set_dpi_awareness();
    let monitors = leopardwm_platform_win32::enumerate_monitors()?;
    let mut windows = Vec::new();
    for index in 0..3 {
        let m = &monitors[index % monitors.len()];
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("LeopardWM import disposable fixture"),
                WS_OVERLAPPEDWINDOW,
                m.work_area.x + 80,
                m.work_area.y + 80,
                640,
                480,
                None,
                None,
                None,
                None,
            )?
        };
        unsafe {
            let _ = ShowWindow(
                hwnd,
                match index {
                    1 => SW_SHOWMAXIMIZED,
                    2 => SW_SHOWMINNOACTIVE,
                    _ => SW_SHOWNOACTIVATE,
                },
            );
        }
        windows.push(hwnd);
    }
    transaction::atomic_write(
        &root.join("ready.json"),
        &serde_json::to_vec(&windows.iter().map(|h| h.0 as u64).collect::<Vec<_>>())?,
    )?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    while std::time::Instant::now() < deadline {
        let mut msg = MSG::default();
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    for hwnd in windows {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
    Ok(())
}
