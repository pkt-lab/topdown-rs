# topdown-rs

Portable Arm topdown performance analysis tool in Rust — a self-contained
reimplementation of [Arm's topdown_tool](https://gitlab.arm.com/telemetry-solution/telemetry-solution/-/tree/main/tools/topdown_tool).

## Why

The original topdown_tool requires Python 3.9+ with `rich`, `pydantic`, and
`jsonschema`. On embedded targets (Yocto, Android, embedded Linux) these
dependencies are rarely available. **topdown-rs** produces a single static
binary (~2-5 MB) with zero runtime dependencies — like
[simpleperf](https://android.googlesource.com/platform/system/extras/+/master/simpleperf)
or [perfetto](https://perfetto.dev).

### Key differences from the Python version

- **Self-contained**: Uses `perf_event_open(2)` syscalls directly — no `perf`
  binary, no `inotifywait`, no Python runtime required
- **Static binary**: Compiles with musl for true zero-dependency deployment
- **Cross-compilation**: First-class support for `aarch64-unknown-linux-musl`
  (Yocto/embedded), `aarch64-linux-android` (Android NDK)
- **Fast**: Native code, no interpreter overhead; particularly significant for
  SPE binary parsing (future)

## Architecture

```
topdown-rs/
├── crates/
│   ├── telemetry-core/     # Library: JSON spec loading, formula eval, event scheduling
│   └── topdown-tool/       # Binary: CLI, perf_event_open backend, workload management
├── data/
│   ├── pmu/cpu/            # Arm telemetry JSON specs (from upstream)
│   └── schema/cpu/         # JSON schema v1.0
├── NOTICE                  # Apache-2.0 attribution to original Arm project
└── LICENSE                 # Apache-2.0
```

### Crates

- **telemetry-core**: Pure Rust library with no platform dependencies. Handles
  JSON telemetry spec deserialization (`serde`), the in-memory telemetry
  database, arithmetic formula evaluation (Pratt parser), and PMU event
  scheduling with bin-packing.

- **topdown-tool**: The CLI binary. Contains the `perf_event_open` backend
  (direct Linux syscall, no `perf` binary), workload management (command
  execution, PID monitoring, system-wide capture), CPU auto-detection via
  sysfs MIDR, and terminal/CSV output.

## Building

```bash
# Native build
cargo build --release

# Static binary for embedded Linux (requires musl target)
rustup target add aarch64-unknown-linux-musl
cargo build --release --target aarch64-unknown-linux-musl

# Android NDK (requires Android NDK toolchain configured)
cargo build --release --target aarch64-linux-android
```

The release binary is optimized for size (`opt-level = "s"`, LTO, stripped).

## Usage

```bash
# Auto-detect CPU and run topdown analysis on a command
topdown-tool -- ./my_workload

# Specify CPU spec manually
topdown-tool --cpu data/pmu/cpu/neoverse_v2_r0p0_pmu.json -- ./my_workload

# System-wide capture (Ctrl+C to stop)
topdown-tool -a

# Monitor existing PID
topdown-tool -p 1234

# Specific cores
topdown-tool -C 0-3 -- ./my_workload

# List available metrics
topdown-tool --list-metrics --cpu data/pmu/cpu/neoverse_v2_r0p0_pmu.json

# List events
topdown-tool --list-events --cpu data/pmu/cpu/neoverse_v2_r0p0_pmu.json

# Specific metric groups
topdown-tool --metric-group Topdown_L1 -- ./my_workload

# Stage selection
topdown-tool --stages 1 -- ./my_workload     # Stage 1 only (topdown)
topdown-tool --stages 2 -- ./my_workload     # Stage 2 only (micro-arch)

# CSV output
topdown-tool --csv ./results -- ./my_workload

# Raw event dump
topdown-tool --dump-events -- ./my_workload
```

## Supported CPUs

All Arm Neoverse and Lumex CPUs with published telemetry specifications:

- Neoverse N1, N2, N3, V1, V2, V3
- Lumex C1-nano, C1-pro, C1-premium, C1-ultra, C1-sme2

## License

Apache-2.0. This is a derivative work of [Arm's Telemetry Solution](https://gitlab.arm.com/telemetry-solution/telemetry-solution)
(Copyright 2022-2025 Arm Limited). See [NOTICE](NOTICE) for attribution details.

## Related Projects

- [Arm Telemetry Solution](https://gitlab.arm.com/telemetry-solution/telemetry-solution) — Original Python implementation
- [simpleperf](https://android.googlesource.com/platform/system/extras/+/master/simpleperf) — Android's native profiler (C++)
- [perfetto](https://perfetto.dev) — System-wide tracing for Android and Linux (C++)
