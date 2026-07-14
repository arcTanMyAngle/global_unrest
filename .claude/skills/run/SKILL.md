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
Reset all state: delete `%LOCALAPPDATA%\LiveEarthSignals\live-earth-signals\data`.

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

A good end-to-end check: click a hotspot city (e.g. the Nairobi marker) and
confirm the right-hand inspector fills with the country name, separate
"Media attention" / "Event data" sections, confidence bar, themes, and
headlines.
