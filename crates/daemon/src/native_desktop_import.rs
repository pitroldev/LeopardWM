//! Explicit offline migration. No IPC/event hooks/tiling are active while moving
//! native windows. The complete replacement layout is prepared before mutation.

#[cfg(test)]
mod tests;
mod transaction;

use crate::config::{Config, WindowAction};
use crate::state::AppState;
use anyhow::{ensure, Context, Result};
use clap::Subcommand;
use leopardwm_platform_win32::{native_desktop_import as native, MonitorInfo, WindowInfo};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use transaction::{Backend, NativeBackend, Paths};

#[derive(Subcommand, Debug, Clone)]
pub enum OfflineCommand {
    /// Preview or perform a one-time Windows desktop to LeopardWM workspace migration.
    ImportNativeDesktops {
        /// Apply the previewed mapping; default is read-only.
        #[arg(long, conflicts_with = "restore")]
        apply: bool,
        /// Keep empty Windows desktops after moving their windows.
        #[arg(long, requires = "apply")]
        keep_native_desktops: bool,
        /// Restore windows and original config/state from an import backup directory.
        #[arg(long)]
        restore: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    window: native::Window,
    workspace: usize,
    floating: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Plan {
    inventory: native::Inventory,
    entries: Vec<Entry>,
    skipped: Vec<(u64, String)>,
    config_after: String,
    state_after: String,
}

fn make_plan(
    config: &Config,
    inventory: native::Inventory,
    monitors: Vec<MonitorInfo>,
) -> Result<Plan> {
    ensure!(
        !inventory.desktops.is_empty() && inventory.desktops.len() <= 9,
        "Import requires 1-9 native desktops; refusing to merge or truncate desktops"
    );
    let ids: HashSet<_> = inventory.desktops.iter().map(|d| &d.id).collect();
    ensure!(
        ids.len() == inventory.desktops.len(),
        "Duplicate native desktop IDs"
    );
    let current_idx = inventory
        .desktops
        .iter()
        .position(|d| d.id == inventory.current)
        .context("Current native desktop missing from inventory")?;
    let mut cfg = config.clone();
    cfg.workspaces.names = inventory.desktops.iter().map(|d| d.name.clone()).collect();
    let mut state = AppState::new_with_config(cfg.clone(), monitors.clone());
    for monitor in &monitors {
        state.ensure_workspace_exists(monitor.id, inventory.desktops.len() - 1);
        state.active_workspace.insert(monitor.id, current_idx);
    }
    let foreground = leopardwm_platform_win32::get_foreground_window();
    let mut windows = inventory.windows.clone();
    windows.sort_by_key(|w| {
        (
            w.desktop.clone(),
            w.monitor.clone(),
            w.rect.x,
            w.rect.y,
            w.hwnd,
        )
    });
    let mut entries = Vec::new();
    let mut skipped = Vec::new();
    let mut seen = HashSet::new();
    for window in windows {
        ensure!(
            seen.insert(window.hwnd),
            "Duplicate HWND in native inventory"
        );
        let index = inventory
            .desktops
            .iter()
            .position(|d| d.id == window.desktop)
            .context("Window references a missing native desktop")?;
        let rule = state.matched_rule(&window.class_name, &window.title, &window.executable);
        let action = rule.map(|r| r.action).unwrap_or(WindowAction::Tile);
        let info = WindowInfo {
            hwnd: window.hwnd,
            title: window.title.clone(),
            class_name: window.class_name.clone(),
            process_id: window.pid,
            rect: window.rect,
            visible: true,
        };
        let reason = if !window.manageable {
            Some("unmanageable or elevated")
        } else if window.pinned {
            Some("pinned on all native desktops")
        } else if action == WindowAction::Ignore {
            Some("ignored by window rule")
        } else {
            state.unmanaged_helper_kind(&info, rule.is_some())
        };
        if let Some(reason) = reason {
            skipped.push((window.hwnd, reason.to_string()));
            continue;
        }
        let monitor = monitors
            .iter()
            .find(|m| m.device_name == window.monitor)
            .context("Monitor changed during import planning")?;
        let width = rule
            .and_then(|r| r.column_width)
            .map(|f| (f * monitor.work_area.width as f64).round() as i32);
        let floating = action == WindowAction::Float;
        let rect = if floating {
            state.get_floating_rect_from_rules(
                &window.class_name,
                &window.title,
                &window.executable,
                &window.rect,
                Some(monitor.id),
            )
        } else {
            window.rect
        };
        let workspace = state.ensure_workspace_exists(monitor.id, index).unwrap();
        if floating {
            workspace.add_floating(window.hwnd, rect)?;
        } else {
            workspace.insert_window(window.hwnd, width)?;
        }
        if window.minimized {
            workspace.mark_minimized(window.hwnd);
        }
        entries.push(Entry {
            window,
            workspace: index,
            floating,
        });
    }
    if let Some(entry) = entries
        .iter()
        .find(|e| foreground == Some(e.window.hwnd) && !e.floating)
    {
        let monitor = monitors
            .iter()
            .find(|m| m.device_name == entry.window.monitor)
            .unwrap();
        state
            .ensure_workspace_exists(monitor.id, entry.workspace)
            .unwrap()
            .focus_window(entry.window.hwnd)?;
        state.focused_monitor = monitor.id;
    }
    for monitor in &monitors {
        for workspace in state.workspaces.get_mut(&monitor.id).unwrap() {
            workspace.ensure_focused_visible(monitor.work_area.width);
        }
    }
    Ok(Plan {
        inventory,
        entries,
        skipped,
        config_after: toml::to_string_pretty(&cfg)?,
        state_after: state.build_state_json()?,
    })
}

pub fn run(command: OfflineCommand) -> Result<()> {
    leopardwm_platform_win32::set_dpi_awareness();
    let paths = Paths::user()?;
    let mut backend = NativeBackend;
    let OfflineCommand::ImportNativeDesktops {
        apply,
        keep_native_desktops,
        restore,
    } = command;
    if let Some(directory) = restore {
        transaction::restore(&directory, &paths, &mut backend)?;
        println!(
            "Native desktop import restored from {}",
            directory.display()
        );
        return Ok(());
    }
    let original = paths.snapshot()?;
    let config = Config::load()?;
    let inventory = backend.inventory()?;
    let plan = make_plan(
        &config,
        inventory,
        leopardwm_platform_win32::enumerate_monitors()?,
    )?;
    for (idx, desktop) in plan.inventory.desktops.iter().enumerate() {
        let count = plan.entries.iter().filter(|e| e.workspace == idx).count();
        println!(
            "Desktop {} {:?} -> workspace {}: {} windows",
            idx + 1,
            desktop.name,
            idx + 1,
            count
        );
    }
    println!(
        "{} windows excluded by rules, pinning or manageability checks",
        plan.skipped.len()
    );
    if !apply {
        println!("Preview only. Apply with: lwm import-native-desktops --apply");
        return Ok(());
    }
    if plan.inventory.desktops.len() == 1 {
        println!("Only one native desktop remains; no import needed. Existing LeopardWM layout preserved.");
        return Ok(());
    }
    let directory = transaction::apply(plan, &paths, &mut backend, keep_native_desktops, original)?;
    println!(
        "Imported. Backup: {}\nStart the imported workspaces with: lwm run",
        directory.display()
    );
    Ok(())
}

/// Held by both normal daemon and offline commands for their entire lifetime.
/// Prevents startup from racing an import between the pipe check and file writes.
pub struct OperationLock(windows::Win32::Foundation::HANDLE);
impl OperationLock {
    pub fn acquire() -> Result<Self> {
        use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
        use windows::Win32::System::Threading::CreateMutexW;
        let scope = leopardwm_ipc::preferred_pipe_name().replace('\\', "_");
        let name: Vec<u16> = format!("Local\\LeopardWM.Import.{scope}\0")
            .encode_utf16()
            .collect();
        unsafe {
            let handle = CreateMutexW(None, false, windows::core::PCWSTR(name.as_ptr()))?;
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(handle);
                anyhow::bail!(
                    "LeopardWM or a native desktop import is already running; stop it first"
                );
            }
            Ok(Self(handle))
        }
    }
}
impl Drop for OperationLock {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}
