// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 pkt-lab contributors

//! CLI and CSV output rendering for topdown metrics.

use crate::cpu::{ComputedGroup, ComputedMetric, TimestampedComputedGroups};
use crate::workload::TimestampedSnapshot;
use anyhow::Result;
use comfy_table::{modifiers::UTF8_ROUND_CORNERS, presets::UTF8_FULL, Attribute, Cell, Color, Table};
use std::collections::HashMap;
use std::path::Path;
use telemetry_core::database::TelemetryDatabase;

fn new_table() -> Table {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL).apply_modifier(UTF8_ROUND_CORNERS);
    table
}

fn is_percent(units: &str) -> bool {
    units.len() >= 7 && units[..7].eq_ignore_ascii_case("percent")
}

fn format_value(val: Option<f64>, units: &str) -> String {
    match val {
        None => "n/a".to_string(),
        Some(v) if is_percent(units) => format!("{v:.2}"),
        Some(v) => format!("{v:.3}"),
    }
}

fn adjust_unit(units: &str) -> &str {
    if is_percent(units) { "%" } else { units }
}

fn format_csv_value(val: Option<f64>) -> String {
    match val {
        Some(v) => format!("{v}"),
        None => String::new(),
    }
}

fn metric_value_cell(cm: &ComputedMetric) -> Cell {
    let value_str = format_value(cm.value, &cm.units);
    let is_pct = is_percent(&cm.units);
    match cm.value {
        Some(v) if is_pct && v > 50.0 => {
            Cell::new(&value_str)
                .fg(Color::Red)
                .add_attribute(Attribute::Bold)
        }
        Some(_) => Cell::new(&value_str),
        None => Cell::new(&value_str).fg(Color::DarkGrey),
    }
}

/// Flatten grouped results into a per-event-name map.
fn flatten_snapshot(results: &HashMap<Vec<String>, Vec<Option<f64>>>) -> HashMap<&str, Option<f64>> {
    let mut map = HashMap::new();
    for (names, values) in results {
        for (name, val) in names.iter().zip(values.iter()) {
            map.insert(name.as_str(), *val);
        }
    }
    map
}

// ─── Listing modes ───────────────────────────────────────────────────────────

pub fn list_events(db: &TelemetryDatabase, descriptions: bool) -> Result<()> {
    let mut table = new_table();

    if descriptions {
        table.set_header(vec!["Event", "Code", "Title", "Description"]);
    } else {
        table.set_header(vec!["Event", "Code", "Title"]);
    }

    let mut events: Vec<_> = db.events.values().collect();
    events.sort_by_key(|e| &e.name);

    for ev in events {
        let mut row = vec![
            Cell::new(&ev.name),
            Cell::new(format!("0x{:04x}", ev.code)),
            Cell::new(&ev.title),
        ];
        if descriptions {
            row.push(Cell::new(&ev.description));
        }
        table.add_row(row);
    }

    println!("{table}");
    println!("\nTotal: {} events", db.events.len());
    Ok(())
}

pub fn list_metrics(db: &TelemetryDatabase, descriptions: bool) -> Result<()> {
    fn print_stage(db: &TelemetryDatabase, label: &str, group_names: &[String], descriptions: bool) {
        println!("=== {label} ===\n");
        for gn in group_names {
            if let Some(group) = db.groups.get(gn) {
                println!("  {} ({})", group.title, group.name);
                for mn in &group.metric_names {
                    if let Some(m) = db.metrics.get(mn) {
                        if descriptions {
                            println!("    - {} [{}]  — {}", m.name, m.units, m.title);
                        } else {
                            println!("    - {} [{}]", m.name, m.units);
                        }
                    }
                }
                println!();
            }
        }
    }

    print_stage(db, "Stage 1 (Topdown)", &db.topdown.stage_1_group_names, descriptions);
    print_stage(db, "Stage 2 (Micro-architecture)", &db.topdown.stage_2_group_names, descriptions);
    println!("Total: {} metrics", db.metrics.len());
    Ok(())
}

pub fn list_groups(db: &TelemetryDatabase, descriptions: bool) -> Result<()> {
    let mut table = new_table();

    if descriptions {
        table.set_header(vec!["Group", "Stage", "Metrics", "Description"]);
    } else {
        table.set_header(vec!["Group", "Stage", "Metrics"]);
    }

    let mut groups: Vec<(&String, &telemetry_core::database::Group)> =
        db.groups.iter().collect();
    groups.sort_by_key(|(n, _)| *n);

    for (name, group) in groups {
        let stage = db.topdown.get_stage_for_group(name);
        let mut row = vec![
            Cell::new(name),
            Cell::new(stage),
            Cell::new(group.metric_names.len()),
        ];
        if descriptions {
            row.push(Cell::new(&group.description));
        }
        table.add_row(row);
    }

    println!("{table}");
    Ok(())
}

// ─── Metric rendering ────────────────────────────────────────────────────────

pub fn render_metrics(
    computed: &[ComputedGroup],
    db: &TelemetryDatabase,
    csv_path: Option<&Path>,
    descriptions: bool,
) -> Result<()> {
    render_metrics_terminal(computed, db, descriptions);
    if let Some(dir) = csv_path {
        write_metrics_csv(computed, dir)?;
    }
    Ok(())
}

fn render_metrics_terminal(
    computed: &[ComputedGroup],
    db: &TelemetryDatabase,
    descriptions: bool,
) {
    for group in computed {
        println!();
        let stage_label = if group.stage == 1 { "Stage 1" } else { "Stage 2" };
        println!("── {} ({}) ──", group.group_title, stage_label);

        let mut table = new_table();

        if descriptions {
            table.set_header(vec!["Metric", "Value", "Unit", "Description"]);
        } else {
            table.set_header(vec!["Metric", "Value", "Unit"]);
        }

        for cm in &group.metrics {
            let mut row = vec![
                Cell::new(&cm.metric_name),
                metric_value_cell(cm),
                Cell::new(adjust_unit(&cm.units)),
            ];

            if descriptions {
                let desc = db
                    .metrics
                    .get(&cm.metric_name)
                    .map(|m| m.title.as_str())
                    .unwrap_or("");
                row.push(Cell::new(desc));
            }

            table.add_row(row);
        }

        println!("{table}");
    }
}

fn write_metrics_csv(computed: &[ComputedGroup], dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("metrics.csv");
    let mut wtr = csv::Writer::from_path(&path)?;
    wtr.write_record(["group", "stage", "metric", "value", "units"])?;

    for group in computed {
        let stage_str = group.stage.to_string();
        for cm in &group.metrics {
            wtr.write_record([
                &group.group_name,
                &stage_str,
                &cm.metric_name,
                &format_csv_value(cm.value),
                &cm.units,
            ])?;
        }
    }

    wtr.flush()?;
    println!("\nCSV written to: {}", path.display());
    Ok(())
}

// ─── Interval metric rendering ───────────────────────────────────────────────

/// Render interval (time-series) metrics to the terminal and optionally to CSV.
pub fn render_interval_metrics(
    intervals: &[TimestampedComputedGroups],
    csv_path: Option<&Path>,
) -> Result<()> {
    if intervals.is_empty() {
        println!("No interval data collected.");
        return Ok(());
    }

    render_interval_metrics_terminal(intervals);

    if let Some(dir) = csv_path {
        write_interval_metrics_csv(intervals, dir)?;
    }

    Ok(())
}

fn render_interval_metrics_terminal(intervals: &[TimestampedComputedGroups]) {
    // Use the first interval's groups as the template for column ordering
    let first = &intervals[0];

    for (group_idx, group) in first.groups.iter().enumerate() {
        println!();
        let stage_label = if group.stage == 1 { "Stage 1" } else { "Stage 2" };
        println!("── {} ({}) ──", group.group_title, stage_label);

        // Build header: Timestamp + each metric name
        let mut header = vec![Cell::new("Timestamp (s)")];
        for cm in &group.metrics {
            let unit_str = adjust_unit(&cm.units);
            header.push(Cell::new(format!("{} ({})", cm.metric_name, unit_str)));
        }

        let mut table = new_table();
        table.set_header(header);

        for ts_group in intervals {
            if group_idx >= ts_group.groups.len() {
                continue;
            }
            let g = &ts_group.groups[group_idx];
            let mut row = vec![Cell::new(format!("{:.3}", ts_group.timestamp))];

            for cm in &g.metrics {
                row.push(metric_value_cell(cm));
            }

            table.add_row(row);
        }

        println!("{table}");
    }
}

fn write_interval_metrics_csv(
    intervals: &[TimestampedComputedGroups],
    dir: &Path,
) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("metrics_interval.csv");
    let mut wtr = csv::Writer::from_path(&path)?;
    wtr.write_record(["timestamp", "group", "stage", "metric", "value", "units"])?;

    for ts_group in intervals {
        let ts_str = format!("{:.3}", ts_group.timestamp);
        for group in &ts_group.groups {
            let stage_str = group.stage.to_string();
            for cm in &group.metrics {
                wtr.write_record([
                    &ts_str,
                    &group.group_name,
                    &stage_str,
                    &cm.metric_name,
                    &format_csv_value(cm.value),
                    &cm.units,
                ])?;
            }
        }
    }

    wtr.flush()?;
    println!("\nCSV written to: {}", path.display());
    Ok(())
}

/// Dump raw event values per interval.
pub fn dump_interval_events(
    snapshots: &[TimestampedSnapshot],
    csv_path: Option<&Path>,
) -> Result<()> {
    if snapshots.is_empty() {
        println!("No interval data collected.");
        return Ok(());
    }

    // Collect all event names in stable order from all snapshots
    let mut event_names: Vec<&str> = snapshots
        .iter()
        .flat_map(|s| s.results.keys().flat_map(|k| k.iter().map(|n| n.as_str())))
        .collect();
    event_names.sort();
    event_names.dedup();

    let mut header = vec![Cell::new("Timestamp (s)")];
    for name in &event_names {
        header.push(Cell::new(*name));
    }

    let mut table = new_table();
    table.set_header(header);

    // Build CSV writer if needed (single pass for both terminal and CSV)
    let mut csv_wtr = if let Some(dir) = csv_path {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("events_interval.csv");
        let mut wtr = csv::Writer::from_path(&path)?;
        let mut csv_header = vec!["timestamp".to_string()];
        csv_header.extend(event_names.iter().map(|n| n.to_string()));
        wtr.write_record(&csv_header)?;
        Some((wtr, path))
    } else {
        None
    };

    for snapshot in snapshots {
        let event_values = flatten_snapshot(&snapshot.results);
        let ts_str = format!("{:.3}", snapshot.timestamp);

        let mut row = vec![Cell::new(&ts_str)];
        let mut csv_record = vec![ts_str.clone()];

        for name in &event_names {
            match event_values.get(name) {
                Some(Some(v)) => {
                    row.push(Cell::new(format!("{v:.0}")));
                    csv_record.push(format!("{v}"));
                }
                _ => {
                    row.push(Cell::new("n/a"));
                    csv_record.push(String::new());
                }
            }
        }

        table.add_row(row);
        if let Some((ref mut wtr, _)) = csv_wtr {
            wtr.write_record(&csv_record)?;
        }
    }

    println!("{table}");

    if let Some((mut wtr, path)) = csv_wtr {
        wtr.flush()?;
        println!("\nCSV written to: {}", path.display());
    }

    Ok(())
}

// ─── Event dump ──────────────────────────────────────────────────────────────

pub fn dump_events(
    results: &HashMap<Vec<String>, Vec<Option<f64>>>,
    db: &TelemetryDatabase,
    csv_path: Option<&Path>,
) -> Result<()> {
    let mut table = new_table();
    table.set_header(vec!["Event", "Code", "Value"]);

    let mut all_events: Vec<(&str, u64, Option<f64>)> = Vec::new();
    for (names, values) in results {
        for (name, val) in names.iter().zip(values.iter()) {
            let code = db.events.get(name).map(|e| e.code).unwrap_or(0);
            all_events.push((name, code, *val));
        }
    }
    all_events.sort_by_key(|(name, _, _)| *name);

    for (name, code, val) in &all_events {
        let val_str = match val {
            Some(v) => format!("{v:.0}"),
            None => "n/a".to_string(),
        };
        table.add_row(vec![
            Cell::new(name),
            Cell::new(format!("0x{code:04x}")),
            Cell::new(val_str),
        ]);
    }

    println!("{table}");

    if let Some(dir) = csv_path {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("events.csv");
        let mut wtr = csv::Writer::from_path(&path)?;
        wtr.write_record(["event", "code", "value"])?;
        for (name, code, val) in &all_events {
            let code_str = format!("0x{code:04x}");
            wtr.write_record([*name, code_str.as_str(), format_csv_value(*val).as_str()])?;
        }
        wtr.flush()?;
        println!("\nCSV written to: {}", path.display());
    }

    Ok(())
}
