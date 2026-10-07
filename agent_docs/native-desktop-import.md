# One-time native desktop migration

## Usage

Use the CLI and daemon from the same build:

```powershell
lwm stop
lwm import-native-desktops              # read-only preview
lwm import-native-desktops --apply      # migrate and remove emptied desktops
lwm run                                # load the imported workspaces
```

For example, Windows desktops Personal, Growth and WellBet become workspaces
1, 2 and 3 with those names on each monitor. A Growth window on the left monitor
stays on that monitor in workspace 2. All migrated windows reside on one native
Windows desktop; LeopardWM controls their workspace visibility. Its workspaces
are independent per monitor, unlike Windows' global desktop switch.

Only run the import once. Normal startup loads its saved layout without merging
workspaces. A completed backup prevents accidental re-import (including when an
excluded window kept another native desktop alive). With only one native desktop,
`--apply` leaves the existing LeopardWM layout untouched.

To retain emptied native desktops, use `--apply --keep-native-desktops`. This does
not duplicate windows or provide continuous synchronization between both systems.

## Backup and recovery

Before moving a window, the command writes `journal.json` under
`%APPDATA%\leopardwm\data\native-desktop-imports\<timestamp-token>`. This private
file contains the original config/state, window placement and desktop mapping.
The command prints its exact directory. Do not publish it: titles and paths may
contain personal information.

Failures before the layout is committed trigger rollback. After interruption or
to undo a successful import, stop LeopardWM and run:

```powershell
lwm import-native-desktops --restore "C:\path\to\backup-directory"
```

Recovery restores the original config/state and surviving windows. It skips dead
or recycled window handles and refuses to overwrite a window moved independently
to a third desktop. Closed applications cannot be reopened from this backup.
Missing native desktops are recreated by name with new IDs; their original order
and wallpaper are not reconstructed. The importer never changes wallpaper.

## Contract and limitations

1. Add an offline `lwm import-native-desktops` preview and explicit `--apply`.
   The daemon must be stopped. Normal startup remains non-migrating.
2. Read the ordered native desktop list and app membership. Preserve native names,
   map desktop N to workspace N on every physical monitor, and select the former
   native current desktop's index. Refuse more than nine desktops; never merge.
3. Build the complete replacement config/workspace snapshot before moving anything.
   Keep rules (tile/float/ignore), monitor ownership and minimized state. Native
   maximized tiled windows are restored without activation when importing.
4. Persist a journal and original config/state before mutation. Stamp HWND lifetimes,
   verify every move, and roll back on failures before committing the new layout.
5. Only after the layout is durable, remove verified empty native desktops, retaining
   the currently active one. Leave desktops containing excluded/unmanageable windows.
   `--keep-native-desktops` retains empty desktops too.
6. Support `--restore BACKUP` while stopped. Recreated Windows desktops receive new
   GUIDs; original native desktop order/wallpaper are not reconstructed. Never change
   wallpaper during migration. Record recovery progress and revalidate HWND identity.
7. Reject concurrent daemon/import processes and config/state edits during planning
   or transfer. Pinned, ignored, helper and elevated windows are excluded; native
   desktops containing excluded windows remain. More than nine desktops aborts
   before mutation. Replacing the config preserves supported settings but rewrites
   TOML formatting/comments; original bytes remain in the backup.

Backend: pinned `winvd` 0.0.49, MIT, from
<https://github.com/Ciantic/VirtualDesktopAccessor/tree/rust/>. It uses private shell
interfaces and requires a compatible Windows 11 build (documented minimum
26100.2605). The explicit preview probes compatibility; errors abort before mutation.
Regular tiling still uses only the existing read-only documented membership API.

## Validation

Deterministic tests exercise names, monitor/workspace membership, exclusions,
minimized state, too many desktops, stale HWNDs, file conflicts, repeated imports,
keep-native behavior and rollback when a native move mutates before returning an
error. Run `pwsh -NoProfile -File tools/check.ps1` for the required repository check.

The opt-in native test uses three disposable windows in a separate process and a
temporary desktop. It exercises the production transaction, cross-process moves,
maximized restoration, minimized preservation, monitor membership, empty desktop
removal and recovery. It restricts mutation to its child process and owned desktops,
uses temporary config/state files and verifies the original desktop list survives:

```powershell
$env:LEOPARDWM_TEST_NATIVE_IMPORT = '1'
cargo test -p leopardwm-daemon native_migration_moves_other_process_windows_and_restores_grouping -- --ignored --nocapture
Remove-Item Env:LEOPARDWM_TEST_NATIVE_IMPORT
```

Passed on Windows 11 build 26220.9587 with three monitors at 100%, 125% and 175% DPI.
Debug builds of the dependency may print `WindowNotFound` for system views during
enumeration; these are excluded. Other enumeration errors abort the operation.
Native API verification is not pixel-level validation of every application.
