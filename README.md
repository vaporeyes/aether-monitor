<!-- ABOUTME: Documents the Aether Monitor native macOS status bar app. -->
<!-- ABOUTME: Covers setup, architecture, verification, and known limitations. -->

# Aether Monitor

Aether Monitor is a native macOS menu bar system monitor inspired by iStat Menus. It is written in Rust and uses AppKit through `objc2`, a custom `NSStatusItem` view for live menu bar sparklines, and a `CAMetalLayer` plus `wgpu` and `egui` for the popover panel.

## Current Features

- Accessory-only macOS menu bar app.
- Live menu bar sparkline with CPU and network activity.
- Popover panel with CPU, memory, network, and temperature telemetry.
- 1 Hz telemetry worker using `sysinfo`.
- Triple-buffered telemetry handoff from worker thread to AppKit UI.
- Main-thread AppKit redraw and Metal render scheduling.

## Requirements

- macOS.
- Rust toolchain with edition 2024 support.
- Xcode Command Line Tools.

## Build And Run

```bash
cargo build --release
target/release/aether_monitor
```

For a bounded smoke run:

```bash
scripts/smoke-run.sh
```

The smoke script builds the debug binary, launches it briefly, then stops it so the AppKit run loop does not keep the command open.

## Verification

Use these commands before submitting changes:

```bash
cargo fmt --check
cargo check --quiet
cargo test --quiet
cargo clippy --quiet -- -D warnings
cargo build --release
```

`cargo test` currently validates buildable test targets only; no unit tests are defined yet.

## Architecture

### `src/telemetry.rs`

Defines `TelemetryFrame` and `TelemetryPipe`.

- `TelemetryFrame` is the shared data contract for UI rendering.
- `TelemetryPipe` wraps `triple_buffer` so the worker can publish frames without blocking the main thread.

### `src/main.rs`

Owns application lifecycle.

- Creates the telemetry pipe.
- Starts the 1 Hz telemetry worker.
- Registers an AppKit `AppDelegate`.
- Builds the status item, menu bar view, popover, canvas view, and view controller.
- Schedules main-thread redraws with Objective-C selectors.

### `src/ui.rs`

Declares AppKit view subclasses.

- `AetherMenuBarView` draws the live menu bar sparklines and opens the popover on click.
- `AetherCanvasView` hosts the Metal-backed popover render surface and forwards pointer input to egui.

### `src/gpu.rs`

Owns the `wgpu` surface and egui renderer.

- Binds directly to the `CAMetalLayer`.
- Uses low-power adapter preference.
- Renders the popover panel from the latest `TelemetryFrame`.

## Telemetry Notes

- CPU and memory come from `sysinfo::System`.
- Network values are per-refresh deltas summed across interfaces.
- Menu bar network activity is logarithmically normalized so low traffic remains visible and large spikes do not flatten the graph.
- Temperature uses the highest finite value exposed by `sysinfo::Components`. On macOS systems that do not expose component sensors through this API, the value remains `0.0 C`.

## Dependency Note

The original implementation spec listed `wgpu = "0.20"`. The pinned `egui-wgpu = "0.27"` renderer depends on `wgpu 0.19`, so this project uses `wgpu = "0.19"` to keep a single compatible GPU type graph. Attempting to use `wgpu 0.20` directly introduces incompatible `Device`, `Queue`, and render-pass types between `wgpu` and `egui-wgpu`.

## Troubleshooting

### Menu bar sparkline does not update

Make sure you are running a freshly built binary:

```bash
cargo build --release
target/release/aether_monitor
```

The worker schedules `redrawTelemetry` on the main thread every second. If an older binary is still running, quit it before starting the new build.

### Popover is blank

Run from the terminal and look for `AetherCanvasView GPU init failed` or `AetherCanvasView render failed`. These messages indicate Metal surface or drawable issues.

### LaunchServices or HIServices messages appear

When launched from a terminal or smoke script, macOS may print LaunchServices or HIServices warnings. The app can still run successfully if the process stays alive and the menu bar item appears.

## Known Limitations

- No quit menu or preferences UI yet.
- No packaged `.app` bundle yet.
- No tests cover AppKit behavior, Metal presentation, or Objective-C selector dispatch.
- The background telemetry worker runs until process exit.
- The menu bar view uses the deprecated `NSStatusItem.setView:` path because the spec calls for a custom status-item view.
