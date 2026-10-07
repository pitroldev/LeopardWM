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
reset. The user explicitly accepted native testing as the fallback. There is no
claim of visual acceptance or application compatibility for browsers, editors or
custom client-side frames. The preview has not been installed or left running;
the existing desktop setup is preserved.
