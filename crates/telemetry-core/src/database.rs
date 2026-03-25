// SPDX-License-Identifier: Apache-2.0
// Copyright 2022-2025 Arm Limited (original Python implementation)
// Copyright 2026 pkt-lab contributors (Rust reimplementation)

//! In-memory telemetry database built from a [`TelemetrySpecification`].
//!
//! Ported from `cpu_telemetry_database.py`. Provides [`Event`], [`Metric`],
//! [`Group`], [`TopdownMethodology`] and [`TelemetryDatabase`] with query APIs.

use crate::spec::TelemetrySpecification;
use std::collections::HashMap;

// ─── Event ───────────────────────────────────────────────────────────────────

/// A hardware PMU event.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Event {
    pub name: String,
    pub title: String,
    pub description: String,
    /// Numeric event code (parsed from hex string like "0x0011").
    pub code: u64,
}

impl Event {
    /// Returns the perf-compatible event name, e.g. `r11` for code 0x0011.
    pub fn perf_name(&self) -> String {
        format!("r{:x}", self.code)
    }
}

impl Ord for Event {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.name.cmp(&other.name)
    }
}

impl PartialOrd for Event {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

// ─── Metric ──────────────────────────────────────────────────────────────────

/// A computed metric derived from one or more events via a formula.
#[derive(Debug, Clone)]
pub struct Metric {
    pub name: String,
    pub title: String,
    pub description: String,
    pub units: String,
    pub formula: String,
    /// Event names required by this metric's formula (sorted).
    pub event_names: Vec<String>,
    /// Sample event names for this metric.
    pub sample_event_names: Vec<String>,
}

impl PartialEq for Metric {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for Metric {}

impl std::hash::Hash for Metric {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.name.hash(state);
    }
}

impl Ord for Metric {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.name.cmp(&other.name)
    }
}

impl PartialOrd for Metric {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

// ─── Group ───────────────────────────────────────────────────────────────────

/// A named group of metrics (e.g., "Topdown_Backend").
#[derive(Debug, Clone)]
pub struct Group {
    pub name: String,
    pub title: String,
    pub description: String,
    /// Metric names in this group (sorted).
    pub metric_names: Vec<String>,
}

impl PartialEq for Group {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for Group {}

impl std::hash::Hash for Group {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.name.hash(state);
    }
}

impl Ord for Group {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.name.cmp(&other.name)
    }
}

impl PartialOrd for Group {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Group {
}

// ─── GroupView ───────────────────────────────────────────────────────────────

/// A filtered view of a [`Group`] with a subset of its metrics.
#[derive(Debug, Clone)]
pub struct GroupView {
    pub group_name: String,
    pub metric_names: Vec<String>,
}

// ─── GroupLike ────────────────────────────────────────────────────────────────

/// Either a full [`Group`] or a [`GroupView`] (filtered subset).
#[derive(Debug, Clone)]
pub enum GroupLike {
    Full(String),     // group name
    View(GroupView),  // filtered view
}

impl GroupLike {
    pub fn group_name(&self) -> &str {
        match self {
            GroupLike::Full(name) => name,
            GroupLike::View(view) => &view.group_name,
        }
    }

    pub fn metric_names<'a>(&'a self, db: &'a TelemetryDatabase) -> &'a [String] {
        match self {
            GroupLike::Full(name) => {
                db.groups.get(name).map(|g| g.metric_names.as_slice()).unwrap_or(&[])
            }
            GroupLike::View(view) => &view.metric_names,
        }
    }

    pub fn all_event_names(&self, db: &TelemetryDatabase) -> Vec<String> {
        let mut events: Vec<String> = self
            .metric_names(db)
            .iter()
            .filter_map(|mn| db.metrics.get(mn))
            .flat_map(|m| m.event_names.iter().cloned())
            .collect();
        events.sort();
        events.dedup();
        events
    }
}

// ─── Topdown Methodology ─────────────────────────────────────────────────────

/// A node in the topdown decision tree.
#[derive(Debug, Clone)]
pub struct TopdownNode {
    pub name: String,
    pub group_name: String,
    pub next_items: Vec<String>,
    pub sample_event_names: Vec<String>,
}

/// The topdown methodology with stage groupings and decision tree.
#[derive(Debug, Clone)]
pub struct TopdownMethodology {
    pub title: String,
    pub description: String,
    pub root_metric_names: Vec<String>,
    pub stage_1_group_names: Vec<String>,
    pub stage_2_group_names: Vec<String>,
    pub nodes: HashMap<String, TopdownNode>,
    /// Maps group name → stage number (1 or 2).
    stage_for_group: HashMap<String, usize>,
    /// Normalized name → original name for fuzzy lookup.
    node_normalized: HashMap<String, String>,
}

impl TopdownMethodology {
    fn build(spec: &TelemetrySpecification) -> Self {
        let td = &spec.methodologies.topdown_methodology;

        let mut stage_for_group = HashMap::new();
        for g in &td.metric_grouping.stage_1 {
            stage_for_group.insert(g.clone(), 1);
        }
        for g in &td.metric_grouping.stage_2 {
            stage_for_group.insert(g.clone(), 2);
        }

        let mut nodes = HashMap::new();
        let mut node_normalized = HashMap::new();
        for n in &td.decision_tree.metrics {
            nodes.insert(
                n.name.clone(),
                TopdownNode {
                    name: n.name.clone(),
                    group_name: n.group.clone(),
                    next_items: n.next_items.clone(),
                    sample_event_names: n.sample_events.clone(),
                },
            );
            node_normalized.insert(normalize_str(&n.name), n.name.clone());
        }

        Self {
            title: td.title.clone(),
            description: td.description.clone(),
            root_metric_names: td.decision_tree.root_nodes.clone(),
            stage_1_group_names: td.metric_grouping.stage_1.clone(),
            stage_2_group_names: td.metric_grouping.stage_2.clone(),
            nodes,
            stage_for_group,
            node_normalized,
        }
    }

    /// Get the stage (1 or 2) for a given group name. Defaults to 2.
    pub fn get_stage_for_group(&self, group_name: &str) -> usize {
        self.stage_for_group.get(group_name).copied().unwrap_or(2)
    }

    /// Find a node by name (case/underscore insensitive).
    pub fn find_node(&self, name: &str) -> Option<&TopdownNode> {
        let normalized = normalize_str(name);
        self.node_normalized
            .get(&normalized)
            .and_then(|orig| self.nodes.get(orig))
    }

}

// ─── TelemetryDatabase ──────────────────────────────────────────────────────

/// The main in-memory database of events, metrics, groups, and topdown methodology.
pub struct TelemetryDatabase {
    pub product_name: String,
    pub num_slots: usize,
    pub events: HashMap<String, Event>,
    pub metrics: HashMap<String, Metric>,
    pub groups: HashMap<String, Group>,
    pub topdown: TopdownMethodology,
    /// Normalized name → original name for fuzzy lookup.
    metrics_normalized: HashMap<String, String>,
    group_normalized: HashMap<String, String>,
}

impl TelemetryDatabase {
    /// Build the database from a validated telemetry specification.
    pub fn from_spec(spec: &TelemetrySpecification) -> Self {
        let mut events = HashMap::new();
        for (name, ev) in &spec.events {
            let code = parse_hex_code(&ev.code);
            events.insert(
                name.clone(),
                Event {
                    name: name.clone(),
                    title: ev.title.clone(),
                    description: ev.description.clone(),
                    code,
                },
            );
        }

        let mut metrics = HashMap::new();
        for (name, m) in &spec.metrics {
            let mut event_names = m.events.clone();
            event_names.sort();
            let mut sample_event_names = m.sample_events.clone();
            sample_event_names.sort();
            metrics.insert(
                name.clone(),
                Metric {
                    name: name.clone(),
                    title: m.title.clone(),
                    description: m.description.clone(),
                    units: m.units.clone(),
                    formula: m.formula.clone(),
                    event_names,
                    sample_event_names,
                },
            );
        }

        let mut groups = HashMap::new();
        for (name, mg) in &spec.groups.metrics {
            let mut metric_names = mg.metrics.clone();
            metric_names.sort();
            groups.insert(
                name.clone(),
                Group {
                    name: name.clone(),
                    title: mg.title.clone(),
                    description: mg.description.clone(),
                    metric_names,
                },
            );
        }

        let metrics_normalized: HashMap<String, String> = metrics
            .keys()
            .map(|k| (normalize_str(k), k.clone()))
            .collect();
        let group_normalized: HashMap<String, String> = groups
            .keys()
            .map(|k| (normalize_str(k), k.clone()))
            .collect();

        let topdown = TopdownMethodology::build(spec);

        Self {
            product_name: spec.product_configuration.product_name.clone(),
            num_slots: spec.product_configuration.num_slots,
            events,
            metrics,
            groups,
            topdown,
            metrics_normalized,
            group_normalized,
        }
    }

    /// Find a group by name (case/underscore insensitive).
    pub fn find_group(&self, name: &str) -> Option<&Group> {
        let normalized = normalize_str(name);
        self.group_normalized
            .get(&normalized)
            .and_then(|orig| self.groups.get(orig))
    }

    /// Find a metric by name (case/underscore insensitive).
    pub fn find_metric(&self, name: &str) -> Option<&Metric> {
        let normalized = normalize_str(name);
        self.metrics_normalized
            .get(&normalized)
            .and_then(|orig| self.metrics.get(orig))
    }

    /// Get the stage number (1 or 2) for a metric.
    pub fn get_metric_stage(&self, metric_name: &str) -> usize {
        for (group_name, group) in &self.groups {
            if group.metric_names.iter().any(|n| n == metric_name) {
                if self.topdown.get_stage_for_group(group_name) == 1 {
                    return 1;
                }
            }
        }
        2
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Normalize a string for case/underscore-insensitive matching.
pub fn normalize_str(s: &str) -> String {
    s.to_lowercase().replace(['_', '-'], "")
}

/// Parse a hex code string like "0x0011" into a u64.
pub fn parse_hex_code(s: &str) -> u64 {
    let stripped = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
    u64::from_str_radix(stripped, 16).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hex_code() {
        assert_eq!(parse_hex_code("0x0011"), 0x11);
        assert_eq!(parse_hex_code("0xd4f"), 0xd4f);
        assert_eq!(parse_hex_code("0x0000"), 0);
    }

    #[test]
    fn test_normalize_str() {
        assert_eq!(normalize_str("Topdown_Backend"), "topdownbackend");
        assert_eq!(normalize_str("backend-bound"), "backendbound");
    }

    #[test]
    fn test_event_perf_name() {
        let ev = Event {
            name: "CPU_CYCLES".into(),
            title: "Cycle".into(),
            description: String::new(),
            code: 0x11,
        };
        assert_eq!(ev.perf_name(), "r11");
    }
}
