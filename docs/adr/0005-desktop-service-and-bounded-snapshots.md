# Desktop service and bounded snapshots

The M1 engine executes individual jobs but does not own a persistent desktop queue.
M2 needs controls that cannot race worker startup, durable settings, and a UI that
does not accumulate progress messages when the webview stops consuming them.

The GUI-independent `DesktopService` contract and generated TypeScript DTOs live
in core. `apps/desktop/service` composes the existing engine, HTTP transport and
SQLite store. Tauri depends on this service and implements only IPC, application
paths, tray actions and notifications. No GUI dependencies enter engine crates.
The separate service package lets fault-server tests exercise queue ownership
without creating a native window or requiring a display server.

Queue admission and controls share a mutex. Pause/cancel await worker quiescence
before resume, rename or removal. Worker lifetimes retain the database lease.
Restart creates a new job, preserving terminal-state invariants and completed
files. Settings use a versioned record in SQLite's existing settings table.

Rust DTOs use ts-rs, a compile-time type exporter with no GUI dependencies. Its
generated export tests update bindings consumed by TypeScript. This is the only
new dependency in core. Serialized byte counts use JavaScript numbers for display;
transfer offsets and checkpoints remain u64 in the engine.

The existing engine event stream remains bounded. Native notifications consume it
with explicit lag handling. The UI requests authoritative snapshot pages with at
most one refresh in flight, at 2 Hz, rather than pushing each engine event into an
unacknowledged webview queue. Each page contains at most 256 jobs. Refresh replaces
the view, so a missed refresh needs no replay. The UI retains at most 2,048 rows and
renders only a virtualized viewport; history pages keep older entries reachable.
Pause-all is a single service operation so repeated IPC round trips cannot compete
with snapshot refreshes. An indexed, bounded admission window excludes terminal
history. A future daemon can implement the same
service contract without changing the view components.

Progress and speed use durable checkpoint deltas.
Non-resumable full streams also expose live, synced byte counts in a worker-owned
map. Those counts are display data and never become recovery checkpoints; weak
validators and missing Range support still require a fresh full transfer.
Worker counts follow live transfer-task guards and return to zero on pause or
cancellation; persisted segment states are shown separately as checkpoint metadata.
Session HTTP diagnostics retain the last successful status and time to headers
for at most 256 jobs. These samples
include connection admission and redirects, and are not advertised as server-only
latency. Response diagnostics are ephemeral; transfer history and retries persist.

Proxy and bandwidth controls are omitted because their engines belong to later
milestones. No shell or filesystem plugin is exposed to the webview. Downloads use
the existing validated Rust path and HTTP boundaries.
Native drag interception is disabled so Windows WebView2 delivers HTML5 drag/drop
events. Imports use browser File objects selected by the user, rather than exposing
arbitrary filesystem reads through a plugin. No core event IPC permissions are
needed because the webview uses bounded snapshot requests.
