// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 pkt-lab contributors

//! CPU detection, spec loading, and metric computation.

use crate::perf;
use crate::Cli;
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use telemetry_core::database::{GroupLike, TelemetryDatabase};
use telemetry_core::formula;
use telemetry_core::spec::TelemetrySpecification;

/// Load the telemetry specification: from --cpu flag or auto-detect.
pub fn load_spec(cli: &Cli) -> Result<TelemetrySpecification> {
    if let Some(ref path) = cli.cpu_spec {
        return TelemetrySpecification::load_from_file(path)
            .map_err(|e| anyhow::anyhow!("{e}"));
    }

    // Auto-detect: read MIDR from core 0 (or first specified core)
    let core = if let Some(ref c) = cli.core {
        parse_core_list(c)?.first().copied().unwrap_or(0)
    } else {
        0
    };

    let midr = perf::read_midr(core)
        .context("Failed to read CPU MIDR. Specify --cpu <file.json> manually.")?;
    let cpu_id = perf::cpu_id_from_midr(midr);

    log::info!("Detected MIDR=0x{midr:08x}, CPU ID=0x{cpu_id:x}");

    // Try to find a matching spec in the metrics directory
    let metrics_dir = resolve_metrics_dir(cli)?;
    let mapping_path = metrics_dir.join("mapping.json");

    if !mapping_path.exists() {
        bail!(
            "No mapping.json found in {}. Use --metrics-dir or --cpu to specify.",
            metrics_dir.display()
        );
    }

    let mapping = telemetry_core::spec::load_cpu_mapping(&mapping_path)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    // Try full MIDR match first, then CPU ID match
    let full_midr_key = format!("0x{midr:x}");
    let cpu_id_key = format!("0x{cpu_id:x}");

    let spec_name = mapping
        .get(&full_midr_key)
        .or_else(|| mapping.get(&cpu_id_key))
        .map(|m| &m.name);

    let spec_name = match spec_name {
        Some(name) => name,
        None => bail!(
            "No telemetry spec found for CPU (MIDR=0x{midr:08x}, ID=0x{cpu_id:x}). \
             Use --cpu <file.json> to specify manually."
        ),
    };

    let spec_path = metrics_dir.join(format!("{spec_name}.json"));
    log::info!("Loading spec: {}", spec_path.display());

    TelemetrySpecification::load_from_file(&spec_path).map_err(|e| anyhow::anyhow!("{e}"))
}

/// Resolve the metrics directory: --metrics-dir, or data/ relative to executable.
fn resolve_metrics_dir(cli: &Cli) -> Result<PathBuf> {
    if let Some(ref dir) = cli.metrics_dir {
        return Ok(dir.clone());
    }

    // Try relative to executable
    if let Ok(exe) = std::env::current_exe() {
        let exe_dir = exe.parent().unwrap_or(Path::new("."));
        let candidates = [
            exe_dir.join("data/pmu/cpu"),
            exe_dir.join("../data/pmu/cpu"),
            exe_dir.join("metrics"),
        ];
        for c in &candidates {
            if c.join("mapping.json").exists() {
                return Ok(c.clone());
            }
        }
    }

    // Try current directory
    let cwd_candidates = [
        PathBuf::from("data/pmu/cpu"),
        PathBuf::from("metrics"),
    ];
    for c in &cwd_candidates {
        if c.join("mapping.json").exists() {
            return Ok(c.clone());
        }
    }

    bail!("Cannot find metrics directory. Use --metrics-dir to specify.")
}

/// Parse the --core argument into a list of core indices.
pub fn parse_cores(cli: &Cli) -> Result<Vec<i32>> {
    if let Some(ref core_str) = cli.core {
        return parse_core_list(core_str);
    }

    // Default: use all online cores
    let n = num_cpus();
    Ok((0..n as i32).collect())
}

fn parse_core_list(s: &str) -> Result<Vec<i32>> {
    let mut cores = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        if let Some((start, end)) = part.split_once('-') {
            let start: i32 = start.trim().parse().context("Invalid core range")?;
            let end: i32 = end.trim().parse().context("Invalid core range")?;
            for i in start..=end {
                cores.push(i);
            }
        } else {
            cores.push(part.parse().context("Invalid core number")?);
        }
    }
    cores.sort();
    cores.dedup();
    Ok(cores)
}

fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

/// List detected CPU cores.
pub fn list_cores() -> Result<()> {
    let n = num_cpus();
    println!("Detected {n} CPU core(s):");
    for i in 0..n {
        let midr = perf::read_midr(i as i32).unwrap_or(0);
        let id = perf::cpu_id_from_midr(midr);
        println!(
            "  Core {i:3}: MIDR=0x{midr:08x}  ID=0x{id:04x}  implementer=0x{:02x} part=0x{:03x}",
            (midr >> 24) & 0xFF,
            (midr >> 4) & 0xFFF
        );
    }
    Ok(())
}

/// Parse --stages argument.
pub fn parse_stages(s: &str) -> Result<Vec<usize>> {
    match s.to_lowercase().as_str() {
        "1" => Ok(vec![1]),
        "2" => Ok(vec![2]),
        "all" | "1,2" | "2,1" => Ok(vec![1, 2]),
        "combined" => Ok(vec![1, 2]),
        _ => bail!("Invalid --stages value: '{s}'. Use 1, 2, all, or combined."),
    }
}

/// Build the list of capture groups based on CLI options.
pub fn build_capture_groups(
    db: &TelemetryDatabase,
    metric_groups: &[String],
    node: Option<&str>,
    stages: &[usize],
) -> Result<Vec<GroupLike>> {
    // If specific metric groups requested
    if !metric_groups.is_empty() {
        let mut groups = Vec::new();
        for name in metric_groups {
            if db.find_group(name).is_some() {
                groups.push(GroupLike::Full(name.clone()));
            } else {
                bail!("Unknown metric group: '{name}'");
            }
        }
        return Ok(groups);
    }

    // If a specific topdown node requested
    if let Some(node_name) = node {
        if let Some(node) = db.topdown.find_node(node_name) {
            let mut groups = vec![GroupLike::Full(node.group_name.clone())];
            // Also add groups referenced in next_items
            for next in &node.next_items {
                if db.groups.contains_key(next) {
                    groups.push(GroupLike::Full(next.clone()));
                }
            }
            return Ok(groups);
        } else {
            bail!("Unknown topdown node: '{node_name}'");
        }
    }

    // Default: all groups for requested stages
    let mut groups = Vec::new();
    if stages.contains(&1) {
        for name in &db.topdown.stage_1_group_names {
            groups.push(GroupLike::Full(name.clone()));
        }
    }
    if stages.contains(&2) {
        for name in &db.topdown.stage_2_group_names {
            groups.push(GroupLike::Full(name.clone()));
        }
    }

    Ok(groups)
}

/// Computed metric value for a single metric.
#[derive(Debug, Clone)]
pub struct ComputedMetric {
    pub metric_name: String,
    pub value: Option<f64>,
    pub units: String,
}

/// Computed metrics grouped by their group.
#[derive(Debug, Clone)]
pub struct ComputedGroup {
    pub group_name: String,
    pub group_title: String,
    pub stage: usize,
    pub metrics: Vec<ComputedMetric>,
}

/// Compute metrics from raw event results.
pub fn compute_metrics(
    db: &TelemetryDatabase,
    capture_groups: &[GroupLike],
    event_results: &HashMap<Vec<String>, Vec<Option<f64>>>,
) -> Result<Vec<ComputedGroup>> {
    // Build a flat event_name → value map from the grouped results
    let mut event_values: HashMap<String, f64> = HashMap::new();
    for (names, values) in event_results {
        for (name, val) in names.iter().zip(values.iter()) {
            if let Some(v) = val {
                // Use the last seen value; perf counters are already aggregated
                // across cores, so accumulating causes double-counting when the
                // same event appears in multiple scheduler groups.
                event_values.insert(name.clone(), *v);
            }
        }
    }

    let mut computed_groups = Vec::new();

    for gl in capture_groups {
        let group_name = gl.group_name().to_string();
        let (title, stage) = if let Some(g) = db.groups.get(&group_name) {
            (g.title.clone(), db.topdown.get_stage_for_group(&group_name))
        } else {
            (group_name.clone(), 2)
        };

        let metric_names = gl.metric_names(db);
        let mut metrics = Vec::new();

        for mn in metric_names {
            if let Some(metric) = db.metrics.get(mn) {
                // Build variable map for this metric's formula
                let vars: HashMap<String, f64> = metric
                    .event_names
                    .iter()
                    .filter_map(|en| event_values.get(en).map(|&v| (en.clone(), v)))
                    .collect();

                let value = if vars.len() == metric.event_names.len() {
                    // All events available — evaluate formula
                    match formula::evaluate(&metric.formula, &vars) {
                        Ok(v) if v.is_nan() => None,
                        Ok(v) => Some(v),
                        Err(e) => {
                            log::warn!("Formula error for metric '{}': {e}", metric.name);
                            None
                        }
                    }
                } else {
                    // Missing events
                    None
                };

                metrics.push(ComputedMetric {
                    metric_name: metric.name.clone(),
                    value,
                    units: metric.units.clone(),
                });
            }
        }

        computed_groups.push(ComputedGroup {
            group_name,
            group_title: title,
            stage,
            metrics,
        });
    }

    Ok(computed_groups)
}
