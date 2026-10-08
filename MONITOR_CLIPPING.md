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

## Native Windows desktop isolation after startup feedback

The user confirmed improved clipping but reported windows from separate Win+Tab
desktops being mixed at startup. Snapshot restoration admitted every live,
manageable saved HWND before enumeration. Consequently it bypassed the existing
shell-cloak admission filter and could target a window on another native desktop.

The fork now checks current Windows desktop membership during snapshot restore,
enumeration and live admission, using the documented `IVirtualDesktopManager`.
Foreground requests repeat the check before restoring minimized windows or
attaching input queues. The COM interface stays within its thread's apartment;
failed queries defer the window and reconnect on the next attempt. Shell-cloaked
windows are excluded, while LeopardWM's own app-cloaked inactive workspaces on
the current native desktop remain restorable. No production path calls
`MoveWindowToDesktop`.

Validation on 2026-10-07:
- A native regression created two disposable framed windows and moved only its
  own second HWND to an already-existing Windows desktop. The old restore
  predicate accepted that HWND; the corrected restore and enumeration rejected
  it, while keeping the local app-cloaked window. A foreground request for the
  foreign HWND was refused without changing foreground, native desktop or rect.
- The optimized full daemon was then started twice, with an isolated config that
  ignored all user windows: once with both fixture HWNDs in a saved snapshot,
  once with no saved workspaces. Each run observed 12 IPC window lists, including
  a refresh halfway through. Only the current-desktop fixture was managed.
  Native readback confirmed the foreign fixture's desktop and rect were unchanged.
- The harness restored the original config and workspace-state files byte for
  byte, verified by SHA-256, and closed the daemon and disposable windows.
- Repository validation passed Clippy, 2,114 workspace tests and the tools tests;
  optimized binaries passed the GUI-subsystem and version checks.

```powershell
$env:LEOPARDWM_TEST_NATIVE_DESKTOPS = '1'
cargo test -p leopardwm-daemon startup_restore_does_not_import_or_focus_another_native_desktop -- --ignored --nocapture
Remove-Item Env:\LEOPARDWM_TEST_NATIVE_DESKTOPS
```

This is startup isolation, not a conversion of Windows desktops into LeopardWM
workspaces. Independent saved LeopardWM layouts for each native desktop and
native-desktop switching throughout a running session remain unsupported.
Computer Use still returned native-pipe error 2; validation uses the previously
authorized native fallback, not compositor screenshots.

The CLI now also contains opt-in live-daemon containment and owned-fixture region
repair regressions. They query production IPC ownership and native window/region
geometry, require actual content clipping, record movement ranges, and exclude
floating, maximized and native move/size operations. An earlier diagnostic run
observed one late 7px overlap with changed geometry; its cause was not established.
The subsequent complete six-Chrome-window run covered all three monitors with
3,300 native samples and no violations, and repaired a removed region in 36ms.
This evidence does not promise absence of every transient compositor artifact.

The final full-daemon repetition after the startup fix moved both Chrome windows
on each of the three monitors (all six HWNDs changed X by over 50px). It completed
3,300 native samples, including 1,686 content-clipped samples, with zero measured
overlaps and zero readback errors. Deliberate clip removal was repaired in 352ms.
An earlier stricter run stopped on Win32 error 6 from `GetWindowRgn`; its cause
was not established. The audit now records readback failures separately and
fails on them instead of losing the report, and rechecks PID after readback to
discard departed window lifetimes. No production clipping code was relaxed.

One full-suite repetition hit the existing asynchronous size-only owner-deferral
assertion in `display_change_regression.rs:330`. The focused rerun and the final
unmodified `tools/check.ps1` run passed (Clippy, 2,114 tests, tools tests). Its
timing sensitivity was not addressed by this startup-desktop change.

## Transient Chrome frame artifact while scrolling

A later user report distinguished a temporary cut border/extra strip during focus
scrolling from a persistent size change. Native sampling on the 125% DPI monitor
observed the region/backdrop switching between clipped and fully visible states,
plus the existing compositor-repair `(w-1 -> w)` pair at animation landing.
Sequential frame/region readback also recorded temporary overcropping; these
measurements are not screenshots or atomic DWM presentation observations.

The initial mitigation was `[animation] scroll_duration_ms = 0`, followed by
`lwm reload`. Scrolling remains available but its movement is instantaneous.
Other animation durations can stay enabled. Smooth scrolling with a changing
native clip remains experimental; this mitigation does not repair its rendering.

Zero duration previously still allocated a completed `ScrollAnimation`, leaving
`Workspace::is_animating()` true until the next tick. The daemon consequently
scheduled an async frame and the unnecessary compositor-repair resize. The core
now commits the target immediately and clears the active animation. Regression
tests cover configured and explicit zero duration, interrupted animations and a
positive explicit override. Both tests failed before the correction and pass
afterward; the repository check passes Clippy, 2,128 tests and tools tests.

The updated full daemon was exercised through eight focus changes involving a
real Chrome window. All 1,000 native samples kept its outer size at 1707x1404
and client size at 1689x1395, eliminating the previous one-pixel resize pair.
Sequential region/position queries still caught three transient mismatches at
instant jumps; they do not establish atomic compositor presentation. Computer
Use again failed with native-pipe error 2, so visual acceptance remains pending.

The user subsequently confirmed that instant scrolling still flashed. A native
regression then reproduced a separate ordering error: the synchronous landing
called `DwmFlush` while the conservative old/new intersection was still installed.
After that barrier a fixture entirely inside its monitor still had 407 pixels
cut from its left edge. Checking only the final return value missed this state.

Placement now finalizes clips immediately after positioning, before the landing
composition barrier. It uses observed geometry and continues to retain the
intersection for pending owner-thread moves. The existing final reconciliation
remains in place for app frame changes during measurement/compositor repair.

The presentation-boundary regression failed before this change and passed after
it on all three monitors, in both LTR and RTL layouts (100%, 125%, 175% DPI).
The isolated Chrome acceptance also checks the region immediately after the
landing barrier, at all four monitor edges and on return fully inside. It passed
on all three monitors. These native checks validate region/position ordering;
they do not capture compositor pixels or prove that all Chromium flicker is gone.

The rebuilt daemon retained all 16 existing window/monitor/workspace memberships.
Two live focus runs sampled the user's Chrome 1,000 times each. Instant scrolling
kept outer/client sizes constant, but sequential queries still observed three
intermediate crops. Animated scrolling still exposed transient crops and the
existing one-pixel compositor nudge. The instant setting was restored after the
comparison; config contents match the pre-test backup. Visual acceptance is
still required, rather than treating these measurements as a flicker-free result.

A separate 5,200-window-sample whole-desktop audit did not pass: it flagged one
Slack window whose outer rectangle exactly matched DISPLAY1 while its recorded
workspace belonged to DISPLAY3. That audit excludes native maximize/floating but
does not identify the daemon's application-fullscreen exemptions. Whether this
was a fullscreen classification or another placement issue was not established;
this result must not be described as clean whole-desktop containment. The scoped
native/Chrome clipping regressions and the full repository check passed.
