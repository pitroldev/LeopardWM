# Monitor clipping fork

Goal: prevent tiled application windows from drawing on another monitor while
keeping their full native size and scrolling layout. [Upstream issue #106](https://github.com/jcardama/LeopardWM/issues/106)
was still open when reviewed on 2026-10-07; upstream's region experiment is test-only.

The experimental implementation is opt-in while application/visual acceptance
is pending. Set `clip_tiled_windows = true` under `[behavior]` in config.toml and
run `lwm reload`. Set it back to false for comparison/troubleshooting.
The same flag is available under Settings > Behavior > Keep tiled windows on
their monitor (experimental). Config reload invalidates native presentation even
when the logical layout is unchanged.
Restoring clips does not change wallpaper, Windows monitor topology or FancyZones.

Implementation:
1. Pass explicit tiled-window owner monitor rectangles to the Win32 placement
   layer. Floating, dragging, maximized and fullscreen windows are exempt.
2. Before moving, install the intersection of the old and destination local
   monitor clips. After a synchronous landing, relax to the actual monitor clip.
   A queued async landing retains the restrictive intersection until it settles.
3. Capture original regions (including empty/complex), frame insets and backdrop
   before modification. Preserve full native geometry for layout measurements.
4. Journal original state before mutation, stamped with a window lifetime token
   and PID/TID. Restore on release, pause, stop, panic and watchdog recovery.
   Never restore a recycled HWND using an old journal.
5. Run targeted geometry/lifecycle and native fixture tests, then tools/check.ps1
   and release build. Desktop acceptance needs both actual monitors, stationary
   partial columns, animated scrolling, float/maximize/fullscreen and recovery.

Compatibility limits to validate:
- Windows regions can change DWM frame presentation, rounding and backdrop.
  This fork prioritizes monitor containment over retaining Mica while clipped.
- GetWindowRgn reports ERROR for both no region and failures. A valid, accessible
  window with zero last-error is treated as having no explicit region. Other
  errors abort clipping; this heuristic is not a proof of absent region.
- App-owned regions changed while clipping are detected on the next placement
  and adopted as the new baseline. Hard-kill recovery uses the last journaled
  baseline, so app changes after the last placement are not observable.
- SetWindowRgn is synchronous and can block in a newly hung application. Placement
  owner probes and recovery deadlines reduce this risk, but cannot eliminate it.

Validation on 2026-10-07:
- Native fixtures passed on two actual 2560x1440 monitors, at 100% and 125% DPI.
  Both LTR and RTL layouts, partial left/right columns, stable outer/client size,
  no false native-minimum feedback, fully visible restoration and native maximize.
- Absent, empty and complex original regions were preserved; a stale ownership
  token could not restore over another lifetime's region.
- Recovery passed in a separate process using only the journal and HWND token.
- Pending moves kept their restrictive region; removing clipping ownership
  restored the original region.
- Full repository validation: `pwsh -NoProfile -File tools/check.ps1` passed
  Clippy, 2,112 workspace tests and the tools tests.

Reproduce the opt-in native fixture (requires two connected monitors):

```powershell
$env:LEOPARDWM_TEST_MONITOR_CLIPPING = '1'
cargo test -p leopardwm-platform-win32 monitor_clipping::tests::native_scrolling_and_journal_recovery -- --ignored --nocapture
Remove-Item Env:\LEOPARDWM_TEST_MONITOR_CLIPPING
```

The fixture uses production placement/recovery functions and creates only its
own non-activating windows. It does not launch the daemon or control existing apps.
Build a portable preview with `cargo build --release`; keep all four executables
from `target/x86_64-pc-windows-msvc/release` together, including the watchdog.

Computer Use native pipe returned OS error 2 after repeated retries and session
reset. The user explicitly accepted native testing as the fallback. These tests
do not establish visual acceptance for everyday applications or custom frames.
The preview is portable and the existing desktop setup is preserved.

## Follow-up after desktop rejection

The user tested 3f32dad with clipping enabled and reported that a window still
invaded another monitor. The daemon and watchdog were stopped cleanly; recovery
left no clip journals. Static Win32 fixture success did not establish app-level
containment. The desktop now has a third 3840x2160 monitor at 175% DPI above the
two 2560x1440 monitors, as recorded by the local daemon log.

Plan: reproduce using an opt-in, isolated blank Chrome process (fresh test-only
profile, PID-scoped ownership); cover stationary readback after placement, region
replacement by the application, cached animation frames and DPI/monitor changes.
Fix the reproduced cause and add a regression, then rerun the repository check
and release build. Keep the desktop daemon stopped during implementation.

Implemented follow-up:
- Acquire clipping ownership on cached placements even if no SetWindowPos is
  needed. The isolated Chrome regression failed before this change (missing
  region after a cache hit) and passed afterwards.
- At the existing 500 ms idle check, inspect native region containment without
  taking the mutation lock. If a region was removed/expanded by an app, invalidate
  native presentation and dispatch a normal bounded apply worker. Healthy windows
  are not moved; pause, animation, display-change and shutdown gates are respected.
- Idle ownership uses the last presentation after successful landing clears the
  pending map; current float/maximize/fullscreen/drag exemptions are rechecked.
- Startup logs now explicitly include the effective `clip_tiled_windows` flag.

Follow-up validation:
- Isolated Chrome passed on all three actual monitors at 100%, 125% and 175%,
  including all four edges, same-rectangle cached policy acquisition and repair
  after simulated application region replacement. Its profile is test-only under
  the ignored root target directory; existing browser processes/profiles are not used.
- A separate opt-in daemon fixture passed through AppState and the real apply
  worker: removing the region from a stationary window caused an unchanged layout
  to dispatch again and restore the clip. The fixture owns one non-activating HWND
  on a pumping thread and does not install global hooks or start the desktop daemon.
- Original LTR/RTL scrolling, original-region and cross-process recovery fixtures
  also passed on the three monitors.
- Final repository validation passed Clippy, 2,114 workspace tests and the tools
  tests with `pwsh -NoProfile -File tools/check.ps1`.
- `cargo fmt --all -- --check`, the optimized release build and both executable
  subsystem/version verification scripts passed for the updated candidate.

```powershell
$env:LEOPARDWM_TEST_CLIP_CHROME_EXE = 'C:\Program Files\Google\Chrome\Application\chrome.exe'
cargo test -p leopardwm-platform-win32 isolated_chrome_stays_contained_after_landing -- --ignored --nocapture
Remove-Item Env:\LEOPARDWM_TEST_CLIP_CHROME_EXE
$env:LEOPARDWM_TEST_MONITOR_CLIPPING = '1'
cargo test -p leopardwm-daemon stationary_repair_dispatches_the_unchanged_layout_through_the_daemon_worker -- --ignored --nocapture
Remove-Item Env:\LEOPARDWM_TEST_MONITOR_CLIPPING
```

These regressions validate the repaired paths, not the reporter's exact desktop
sequence or pixel-level DWM presentation. Full desktop acceptance remains pending.
The preview remains experimental and the desktop daemon remains stopped.
