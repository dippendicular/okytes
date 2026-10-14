# Scheduler benchmarks

This harness uses a deterministic loopback origin and a separate download worker.
The origin reuses `crates/test-support`; generated content is checked with SHA-256.
It is a developer tool and is not linked into production applications.

```sh
cargo build -p okytes-bench --release --locked
cargo run -p okytes-bench --release --locked -- run --runs 3 --mode fixed --output benchmarks/results/my-baseline
cargo run -p okytes-bench --release --locked -- run --runs 3 --mode both --output benchmarks/results/my-comparison
```

Use a new output directory for every invocation. The driver refuses to overwrite
raw results. Run on an idle machine, on the same filesystem, without changing
the build profile or origin parameters between comparisons. Mode order alternates
between repetitions. The fixed control uses four workers; both modes share four
connections per origin, four concurrent jobs and 256 KiB request quanta.

Scenarios cover an 8 MiB file over a shared link cap, twenty 64 KiB files,
per-connection throttling, delayed response headers, periodic mid-body resets,
and an origin returning 429 above two active bodies. Seeds, caps, delays, body
sizes and retry conditions are stored with each result. These are controlled
workloads, not claims about public Internet servers.

Each run records effective payload throughput including retries, sync/checkpoint
cost and checksum verification; worker initialization/queue-creation time; completion rate;
sampled process CPU milliseconds and peak RSS; origin peak active bodies; and raw
20 ms process observations. Actual sampling intervals are retained. CPU and RSS
exclude the origin, which runs in the driver process. Sampled peak RSS can miss
short peaks. CPU is cumulative CPU time, not an instantaneous percent. Process
metrics use [sysinfo's process API](https://docs.rs/sysinfo/0.39.6/sysinfo/struct.Process.html).

The flaky scenario also pauses after durable progress, recreates the engine from
SQLite, resumes and verifies the final hash. This measures restart-resume
correctness; forced process termination remains covered by daemon/CLI integration
tests. Other scenarios report resume correctness as null rather than implying a
resume check took place.

`environment.json` captures OS/kernel/architecture, CPU, memory, disk kind and
mounts, compiler versions, Git commit and dirty status. `summary.json` reports run
count, arithmetic mean, population standard deviation, coefficient of variation
and completion rate. Inspect every raw run before interpreting a summary. No
other download manager is compared unless it was actually run under identical
conditions with its version and raw results recorded.

Final runs also capture process startup-to-ready time, retained resume checkpoint
bytes, the worker executable SHA-256, source hashes and source snapshots. The
initial before-tuning archive predates these additional provenance fields.
Worker files use the OS temporary directory, recorded in the final environment.

Linux/macOS execution and filesystem performance vary and require their own
measurements. Do not extrapolate one machine's results to other machines or to
production workloads.

Recorded Windows results and explanations of every regression are in the
[M5 report](../docs/reports/M5.md), with [raw final measurements](results/2026-10-14-m5/summary.json).
