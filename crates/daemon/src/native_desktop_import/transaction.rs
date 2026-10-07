use super::*;
use anyhow::{bail, Context};
use std::fs;
use std::hash::{BuildHasher, Hasher};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(super) trait Backend {
    fn inventory(&mut self) -> Result<native::Inventory>;
    fn stamp(&mut self, window: &native::Window, token: u64) -> Result<()>;
    fn same_lifetime(&mut self, window: &native::Window, token: u64) -> bool;
    fn desktop(&mut self, window: &native::Window) -> Result<String>;
    fn move_window(
        &mut self,
        window: &native::Window,
        token: u64,
        source: &str,
        target: &str,
    ) -> Result<()>;
    fn normalize(&mut self, window: &native::Window, token: u64) -> Result<()>;
    fn restore_placement(&mut self, window: &native::Window, token: u64) -> Result<()>;
    fn remove_empty(&mut self, id: &str, fallback: &str) -> Result<bool>;
    fn create(&mut self, name: &str) -> Result<String>;
}

pub(super) struct NativeBackend;
impl Backend for NativeBackend {
    fn inventory(&mut self) -> Result<native::Inventory> {
        native::inventory()
    }
    fn stamp(&mut self, w: &native::Window, t: u64) -> Result<()> {
        native::stamp(w, t)
    }
    fn same_lifetime(&mut self, w: &native::Window, t: u64) -> bool {
        native::same_lifetime(w, t)
    }
    fn desktop(&mut self, w: &native::Window) -> Result<String> {
        native::window_desktop(w.hwnd)
    }
    fn move_window(&mut self, w: &native::Window, t: u64, from: &str, to: &str) -> Result<()> {
        native::move_window(w, t, from, to)
    }
    fn normalize(&mut self, w: &native::Window, token: u64) -> Result<()> {
        ensure!(
            native::same_lifetime(w, token),
            "Window lifetime changed before normalization"
        );
        if !leopardwm_platform_win32::is_window_maximized(w.hwnd) {
            return Ok(());
        }
        // The existing bounded style worker requires its own lifetime stamp.
        // Our separate migration token remains the recovery authority.
        leopardwm_platform_win32::stamp_managed_lifetime_token(w.hwnd)?;
        leopardwm_platform_win32::queue_maximized_window_restore(w.hwnd)?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while leopardwm_platform_win32::is_window_maximized(w.hwnd) {
            ensure!(
                native::same_lifetime(w, token) && Instant::now() < deadline,
                "Maximized window did not restore"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }
    fn restore_placement(&mut self, w: &native::Window, t: u64) -> Result<()> {
        native::restore_placement(w, t)
    }
    fn remove_empty(&mut self, id: &str, fallback: &str) -> Result<bool> {
        native::remove_empty_desktop(id, fallback)
    }
    fn create(&mut self, name: &str) -> Result<String> {
        native::create_desktop(name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct Paths {
    pub config: PathBuf,
    pub state: PathBuf,
    pub backups: PathBuf,
}
impl Paths {
    pub fn snapshot(&self) -> Result<OriginalFiles> {
        Ok(OriginalFiles {
            config: read_optional(&self.config)?,
            state: read_optional(&self.state)?,
        })
    }

    pub fn user() -> Result<Self> {
        let choices = crate::config::config_paths();
        let config = choices
            .iter()
            .find(|p| p.exists())
            .or_else(|| choices.first())
            .context("No config path")?
            .clone();
        let state = AppState::state_file_path();
        let backups = state
            .parent()
            .context("No state directory")?
            .join("native-desktop-imports");
        Ok(Self {
            config,
            state,
            backups,
        })
    }
}

#[derive(PartialEq, Eq)]
pub(super) struct OriginalFiles {
    config: Option<Vec<u8>>,
    state: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Journal {
    version: u32,
    phase: String,
    token: u64,
    paths: Paths,
    plan: Plan,
    original_config: Option<Vec<u8>>,
    original_state: Option<Vec<u8>>,
    recreated: HashMap<String, String>,
    removed: Vec<String>,
    warnings: Vec<String>,
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub(super) fn atomic_write(path: &Path, content: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().context("Missing output parent")?;
    fs::create_dir_all(parent)?;
    let tmp = path.with_extension(format!("{}.import-tmp", std::process::id()));
    let mut file = fs::File::create(&tmp)?;
    file.write_all(content)?;
    file.sync_all()?;
    drop(file);
    fs::rename(tmp, path)?;
    Ok(())
}

fn save(dir: &Path, journal: &Journal) -> Result<()> {
    atomic_write(
        &dir.join("journal.json"),
        &serde_json::to_vec_pretty(journal)?,
    )
}

fn write_optional(path: &Path, bytes: &Option<Vec<u8>>) -> Result<()> {
    if let Some(bytes) = bytes {
        atomic_write(path, bytes)
    } else if path.exists() {
        fs::remove_file(path).map_err(Into::into)
    } else {
        Ok(())
    }
}

fn verify_original_files(j: &Journal) -> Result<()> {
    ensure!(
        read_optional(&j.paths.config)? == j.original_config
            && read_optional(&j.paths.state)? == j.original_state,
        "Config or workspace state changed during migration; refusing to overwrite it"
    );
    Ok(())
}

pub(super) fn apply(
    plan: Plan,
    paths: &Paths,
    backend: &mut dyn Backend,
    keep: bool,
    original: OriginalFiles,
) -> Result<PathBuf> {
    ensure!(
        paths.snapshot()? == original,
        "Config or workspace state changed while planning; run the import again"
    );
    let live = backend.inventory()?;
    ensure!(
        live.desktops == plan.inventory.desktops && live.current == plan.inventory.current,
        "Native desktops changed after preview; run the import again"
    );
    fs::create_dir_all(&paths.backups)?;
    // An incomplete journal must be recovered before another import can replace
    // its HWND tokens or obscure its recovery instructions.
    for entry in fs::read_dir(&paths.backups)? {
        let path = entry?.path().join("journal.json");
        if !path.is_file() {
            continue;
        }
        let previous: Journal = serde_json::from_slice(&fs::read(&path)?)?;
        ensure!(previous.phase != "complete",
            "Native desktops were already imported at {}; restore that backup before importing again", path.display());
        ensure!(
            previous.phase == "restored",
            "Unfinished import at {}; restore it first",
            path.display()
        );
    }
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos());
    let token = hasher.finish().max(1);
    let dir = paths.backups.join(format!(
        "{}-{token:x}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs()
    ));
    fs::create_dir(&dir)?;
    let mut journal = Journal {
        version: 1,
        phase: "prepared".into(),
        token,
        paths: paths.clone(),
        plan,
        original_config: original.config,
        original_state: original.state,
        recreated: HashMap::new(),
        removed: Vec::new(),
        warnings: Vec::new(),
    };
    save(&dir, &journal)?;
    println!("Backup written before moving windows: {}", dir.display());
    if let Err(error) = transfer(&dir, &mut journal, backend) {
        journal
            .warnings
            .push(format!("Import interrupted: {error:#}"));
        let recovery = restore_journal(&dir, &mut journal, backend, false);
        save(&dir, &journal)?;
        bail!(
            "Import failed: {error:#}. Rollback: {recovery:?}. Backup: {}",
            dir.display()
        );
    }
    if !keep {
        for desktop in journal.plan.inventory.desktops.clone() {
            if desktop.id == journal.plan.inventory.current {
                continue;
            }
            match backend.remove_empty(&desktop.id, &journal.plan.inventory.current) {
                Ok(true) => journal.removed.push(desktop.id),
                Ok(false) => journal
                    .warnings
                    .push(format!("Kept nonempty native desktop {:?}", desktop.name)),
                Err(error) => journal
                    .warnings
                    .push(format!("Kept native desktop {:?}: {error:#}", desktop.name)),
            }
            save(&dir, &journal)?;
        }
    }
    journal.phase = "complete".into();
    save(&dir, &journal)?;
    for warning in &journal.warnings {
        eprintln!("{warning}");
    }
    Ok(dir)
}

fn transfer(dir: &Path, j: &mut Journal, backend: &mut dyn Backend) -> Result<()> {
    verify_original_files(j)?;
    for entry in &j.plan.entries {
        ensure!(
            backend.desktop(&entry.window)? == entry.window.desktop,
            "A window changed desktops before import"
        );
        backend.stamp(&entry.window, j.token)?;
    }
    j.phase = "moving".into();
    save(dir, j)?;
    for entry in &j.plan.entries {
        if entry.window.desktop != j.plan.inventory.current {
            backend.move_window(
                &entry.window,
                j.token,
                &entry.window.desktop,
                &j.plan.inventory.current,
            )?;
        }
        if !entry.floating && !entry.window.minimized {
            backend.normalize(&entry.window, j.token)?;
        }
    }
    for entry in &j.plan.entries {
        ensure!(
            backend.same_lifetime(&entry.window, j.token)
                && backend.desktop(&entry.window)? == j.plan.inventory.current,
            "Window membership changed before layout commit"
        );
    }
    let now = backend.inventory()?;
    ensure!(
        now.current == j.plan.inventory.current && now.desktops == j.plan.inventory.desktops,
        "Native desktops changed while importing; rolling back"
    );
    verify_original_files(j)?;
    atomic_write(&j.paths.config, j.plan.config_after.as_bytes())?;
    atomic_write(&j.paths.state, j.plan.state_after.as_bytes())?;
    j.phase = "layout_saved".into();
    save(dir, j)
}

pub(super) fn restore(dir: &Path, paths: &Paths, backend: &mut dyn Backend) -> Result<()> {
    let mut journal: Journal = serde_json::from_slice(&fs::read(dir.join("journal.json"))?)?;
    ensure!(
        journal.version == 1 && &journal.paths == paths,
        "Backup belongs to a different configuration or version"
    );
    if journal.phase == "restored" {
        return Ok(());
    }
    restore_journal(dir, &mut journal, backend, true)
}

fn restore_journal(
    dir: &Path,
    j: &mut Journal,
    backend: &mut dyn Backend,
    explicit: bool,
) -> Result<()> {
    j.phase = "restoring".into();
    save(dir, j)?;
    let inventory = backend.inventory()?;
    for desktop in j.plan.inventory.desktops.clone() {
        let mapped = j.recreated.get(&desktop.id).unwrap_or(&desktop.id);
        if inventory.desktops.iter().any(|d| &d.id == mapped) {
            continue;
        }
        ensure!(
            explicit,
            "Original native desktop disappeared; explicit recovery required"
        );
        let recreated = backend.create(&desktop.name)?;
        j.recreated.insert(desktop.id, recreated);
        save(dir, j)?;
    }
    for entry in j.plan.entries.iter().rev() {
        if !backend.same_lifetime(&entry.window, j.token) {
            continue;
        }
        let source = backend.desktop(&entry.window)?;
        let target = j
            .recreated
            .get(&entry.window.desktop)
            .unwrap_or(&entry.window.desktop);
        ensure!(
            &source == target || source == j.plan.inventory.current,
            "Window was moved independently after import; refusing to overwrite its desktop"
        );
        if &source != target {
            backend.move_window(&entry.window, j.token, &source, target)?;
        }
        backend.restore_placement(&entry.window, j.token)?;
    }
    if !explicit {
        for (path, before, after) in [
            (
                &j.paths.config,
                &j.original_config,
                j.plan.config_after.as_bytes(),
            ),
            (
                &j.paths.state,
                &j.original_state,
                j.plan.state_after.as_bytes(),
            ),
        ] {
            let current = read_optional(path)?;
            ensure!(
                current == *before || current.as_deref() == Some(after),
                "External file change prevents automatic rollback"
            );
        }
    }
    write_optional(&j.paths.config, &j.original_config)?;
    write_optional(&j.paths.state, &j.original_state)?;
    j.phase = "restored".into();
    save(dir, j)
}
