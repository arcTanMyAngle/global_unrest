---
name: run
description: Launch the Live Earth Signals desktop app (offline fixture mode) and visually verify it on this Windows machine — window screenshot and scripted clicks included. Use when asked to run, demo, screenshot, or confirm the app works.
---

# Running the desktop app

```powershell
cd c:\Users\bornt\Desktop\whats_overhead\live-earth-signals
cargo run -p global-signal-desktop        # must run from workspace root (finds ./fixtures)
```

No network needed. Env overrides: `LES_FIXTURES_DIR`, `LES_DATA_DIR`,
`RUST_LOG` (e.g. `info`), `WGPU_BACKEND` (dx12/vulkan/gl) for driver issues.
M3 live-mode knobs: `LES_ONLINE=1` (auto-start live GDELT), `LES_RETENTION_DAYS`,
`LES_GDELT_DOC_ENDPOINT` / `LES_GDELT_EVENTS_URL` (point the loop at a mock).
Reset all state: delete `%LOCALAPPDATA%\LiveEarthSignals\live-earth-signals\data`.

## Verify M3 graceful degradation headlessly (no clicks)

The network-kill path is verifiable **without synthetic input**: auto-start
online mode and point the loop at a dead port, then confirm the app keeps its
cached fixtures and logs a degraded state.

```powershell
$env:LES_ONLINE = "1"
$env:LES_GDELT_DOC_ENDPOINT = "http://127.0.0.1:9/api/v2/doc/doc"
$env:LES_GDELT_EVENTS_URL   = "http://127.0.0.1:9/gdeltv2/lastupdate.txt"
$env:RUST_LOG = "info"
# launch as above, Start-Sleep 8, then grep the stderr log:
#   expect "ingest complete inserted=11043" (cached data present) and
#   WARN "gdelt fetch failed; degraded, showing cached data … retry_in_s=…"
```

Success (real GDELT reachable) would instead log `gdelt cycle ok records=…`.

Expected startup logs (RUST_LOG=info): basemap tessellated (~10.6k
vertices), fixtures fetched (3 files, ~11k records), fixtures normalized
(2 failures — planted malformed records), ingest complete.

## Headless launch with captured logs

```powershell
$env:RUST_LOG = "info"
$proc = Start-Process -FilePath ".\target\debug\global-signal-desktop.exe" `
  -WorkingDirectory (Get-Location) -PassThru -RedirectStandardError "$env:TEMP\les_app_err.log"
Start-Sleep -Seconds 14   # startup + ingest
# check: $proc.HasExited, Get-Content $env:TEMP\les_app_err.log -Tail 30
```

## Screenshot / click verification (this machine: 2560×1600, DPI-scaled)

Hard-won gotchas — follow exactly:

1. `FindWindow($null, "Live Earth Signals")` **fails** from PowerShell
   (null-string marshaling). Get the handle via
   `(Get-Process -Id $proc.Id).MainWindowHandle`.
2. Call `SetProcessDPIAware()` (user32 via Add-Type) **before any** Win32
   capture/cursor call, or coordinates are DPI-virtualized and captures come
   out clipped/scaled.
3. Capture the **full screen** (2560×1600) with
   `System.Drawing.Graphics::CopyFromScreen`, save PNG to the scratchpad
   dir, then Read the PNG to inspect.
4. Simulate clicks with `SetCursorPos(x, y)` (physical px) +
   `mouse_event(0x0002)` / `mouse_event(0x0004)` in the same DPI-aware
   process; `SetForegroundWindow(handle)` first, small sleeps between steps.
5. Kill when done: `Stop-Process -Id $proc.Id -Force -Confirm:$false`.

Additional lessons from the M2 verification session:

6. The eframe window can open **small and offset**, not maximized —
   `[Win32]::ShowWindow($h, 3)` (SW_SHOWMAXIMIZED) first, take a fresh
   screenshot, and only then compute click coordinates. Never reuse
   coordinates from a previous launch.
7. `SetForegroundWindow` is blocked by Windows focus-stealing prevention
   when another app is active. Working recipe: `ShowWindow($h, 6)`
   (minimize), then wrap `SetForegroundWindow` + `ShowWindow($h, 3)` in an
   Alt keypress (`keybd_event(0x12, 0, 0/2, …)`). **Verify** with
   `GetForegroundWindow() -eq $h` immediately before every synthetic click
   and before every screenshot — otherwise clicks land in whatever the user
   has open.
8. **If foreground keeps being stolen, the user is actively using the
   machine — stop sending input immediately** and fall back to the headless
   E2E test for verification.
9. Each PowerShell tool invocation is a fresh process: `Add-Type` for the
   Win32 helpers must be re-run in every call (types don't persist).

A good end-to-end check: click a hotspot city (e.g. the Nairobi marker) and
confirm the right-hand inspector fills with the country name, separate
"Media attention" / "Event data" sections, the four M2 score bars
(attention / unrest / spike / combined) with cold-start badge on early-day
windows, confidence bar, themes, and headlines.
