// SPDX-License-Identifier: Apache-2.0
// Copyright 2022-2025 Arm Limited (original Python implementation)
// Copyright 2026 pkt-lab contributors (Rust reimplementation)

mod perf;
mod cpu;
mod workload;
mod output;

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "topdown-tool",
    about = "Portable Arm topdown performance analysis tool.\n\n\
             A self-contained reimplementation of Arm's topdown_tool in Rust.\n\
             Uses perf_event_open directly — no Python, no perf binary required.",
    version
)]
struct Cli {
    /// Path to CPU telemetry specification JSON file.
    /// If not specified, auto-detects CPU and uses built-in mappings.
    #[arg(long = "cpu", value_name = "FILE")]
    cpu_spec: Option<PathBuf>,

    /// Directory containing metric JSON files and mapping.json.
    #[arg(long = "metrics-dir", value_name = "DIR")]
    metrics_dir: Option<PathBuf>,

    /// CPU core(s) to monitor (e.g., "0", "0-3", "0,2,4").
    #[arg(long, short = 'C')]
    core: Option<String>,

    /// Run in system-wide mode (all cores, no specific workload).
    #[arg(long, short = 'a')]
    system_wide: bool,

    /// PID(s) to monitor (comma-separated).
    #[arg(long, short = 'p')]
    pid: Option<String>,

    /// Sampling interval in milliseconds.
    #[arg(long, short = 'I', value_name = "MS")]
    interval: Option<u64>,

    /// Event collection strategy: none, metric, or group.
    #[arg(long, default_value = "metric")]
    collect_by: String,

    /// Metric group(s) to capture (comma-separated).
    #[arg(long = "metric-group")]
    metric_group: Vec<String>,

    /// Topdown decision tree node to start from.
    #[arg(long)]
    node: Option<String>,

    /// Topdown stage(s): 1, 2, all, or combined.
    #[arg(long, default_value = "all")]
    stages: String,

    /// List detected CPU cores and exit.
    #[arg(long = "list-cores")]
    list_cores: bool,

    /// List available metric groups and exit.
    #[arg(long = "list-groups")]
    list_groups: bool,

    /// List available metrics and exit.
    #[arg(long = "list-metrics")]
    list_metrics: bool,

    /// List all PMU events and exit.
    #[arg(long = "list-events")]
    list_events: bool,

    /// Dump raw event counts instead of computed metrics.
    #[arg(long = "dump-events")]
    dump_events: bool,

    /// Write CSV output to the specified directory.
    #[arg(long = "csv", value_name = "DIR")]
    csv_output: Option<PathBuf>,

    /// Disable event multiplexing (fail if too many events).
    #[arg(long = "no-multiplex")]
    no_multiplex: bool,

    /// Show descriptions in listing output.
    #[arg(long)]
    descriptions: bool,

    /// Command to run and profile.
    #[arg(trailing_var_arg = true)]
    command: Vec<String>,
}

fn main() -> Result<()> {
    env_logger::init();
    let cli = Cli::parse();

    // Handle list-cores early (doesn't need a spec)
    if cli.list_cores {
        return cpu::list_cores();
    }

    // Auto-detect CPU or load user-specified spec
    let spec = cpu::load_spec(&cli)?;
    let db = telemetry_core::database::TelemetryDatabase::from_spec(&spec);

    // Handle listing modes (no capture)
    if cli.list_events {
        return output::list_events(&db, cli.descriptions);
    }
    if cli.list_metrics {
        return output::list_metrics(&db, cli.descriptions);
    }
    if cli.list_groups {
        return output::list_groups(&db, cli.descriptions);
    }

    // Parse core selection
    let cores = cpu::parse_cores(&cli)?;

    // Check perf privileges
    perf::check_perf_privilege()
        .context("Insufficient privileges for perf_event_open. Try running as root or set /proc/sys/kernel/perf_event_paranoid to -1")?;

    // Determine workload mode
    let workload_mode = workload::WorkloadMode::from_cli(&cli)?;

    // Parse collection strategy
    let collect_by = telemetry_core::scheduler::CollectBy::from_str(&cli.collect_by)
        .unwrap_or(telemetry_core::scheduler::CollectBy::Metric);

    // Build capture groups
    let stages = cpu::parse_stages(&cli.stages)?;
    let capture_groups =
        cpu::build_capture_groups(&db, &cli.metric_group, cli.node.as_deref(), &stages)?;

    // Build event scheduler
    let event_tuples: Vec<Vec<Vec<String>>> = capture_groups
        .iter()
        .map(|gl| {
            gl.metric_names(&db)
                .iter()
                .filter_map(|mn| db.metrics.get(mn))
                .map(|m| m.event_names.clone())
                .collect()
        })
        .collect();

    let max_events = perf::detect_pmu_counters(cores.first().copied().unwrap_or(0))
        .unwrap_or(db.num_slots);

    let scheduler = telemetry_core::scheduler::EventScheduler::new(
        event_tuples,
        collect_by,
        max_events,
    )
    .context("Failed to schedule events")?;

    // Run capture
    let chunks = scheduler.get_event_group_chunks(true);
    log::info!(
        "Capturing {} event group(s) in {} round(s) on {} core(s)",
        scheduler.optimized_event_groups.len(),
        chunks.len(),
        cores.len()
    );

    let mut all_results = std::collections::HashMap::new();

    for (round_idx, chunk) in chunks.iter().enumerate() {
        log::info!("Round {}/{}", round_idx + 1, chunks.len());

        // Resolve event names to codes
        let event_configs: Vec<Vec<perf::EventConfig>> = chunk
            .iter()
            .map(|group| {
                group
                    .iter()
                    .filter_map(|name| {
                        db.events.get(name).map(|ev| perf::EventConfig {
                            name: ev.name.clone(),
                            code: ev.code,
                        })
                    })
                    .collect()
            })
            .collect();

        let results = workload::run_capture(
            &workload_mode,
            &event_configs,
            &cores,
            cli.interval,
        )?;

        // Merge results
        for (group_key, values) in results {
            all_results.insert(group_key, values);
        }
    }

    // Compute metrics
    if cli.dump_events {
        output::dump_events(&all_results, &db, cli.csv_output.as_deref())?;
    } else {
        let computed = cpu::compute_metrics(&db, &capture_groups, &all_results)?;
        output::render_metrics(&computed, &db, cli.csv_output.as_deref(), cli.descriptions)?;
    }

    Ok(())
}
