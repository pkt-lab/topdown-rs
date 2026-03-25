// SPDX-License-Identifier: Apache-2.0
// Copyright 2022-2025 Arm Limited (original Python implementation)
// Copyright 2026 pkt-lab contributors (Rust reimplementation)

//! JSON telemetry specification models.
//!
//! These structs mirror the Arm telemetry JSON schema (v1.0) and are deserialized
//! from files like `neoverse_v2_r0p0_pmu.json`. Ported from `cpu_model.py`.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SpecError {
    #[error("failed to read file {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid JSON in {path}: {source}")]
    Json {
        path: String,
        source: serde_json::Error,
    },
    #[error("validation error: {0}")]
    Validation(String),
}

/// Deserialize a string that may be either a JSON string or number into an integer.
/// The Arm JSON specs encode major_revision/minor_revision as strings (e.g., "0").
fn deserialize_string_or_int<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    struct StringOrInt;

    impl<'de> de::Visitor<'de> for StringOrInt {
        type Value = i64;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a string or integer")
        }

        fn visit_i64<E: de::Error>(self, v: i64) -> Result<i64, E> {
            Ok(v)
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<i64, E> {
            Ok(v as i64)
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<i64, E> {
            v.parse::<i64>().map_err(de::Error::custom)
        }
    }

    deserializer.deserialize_any(StringOrInt)
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProductConfiguration {
    pub product_name: String,
    pub part_num: String,
    pub implementer: String,
    #[serde(deserialize_with = "deserialize_string_or_int")]
    pub major_revision: i64,
    #[serde(deserialize_with = "deserialize_string_or_int")]
    pub minor_revision: i64,
    pub num_slots: usize,
    pub num_bus_slots: usize,
    pub architecture: String,
    pub pmu_architecture: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Event {
    pub code: String,
    pub title: String,
    pub description: String,
    pub architecture_defined: bool,
    pub product_defined: bool,
    pub accesses: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Metric {
    pub title: String,
    pub formula: String,
    pub description: String,
    pub units: String,
    pub events: Vec<String>,
    pub sample_events: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FunctionGroup {
    pub title: String,
    pub description: String,
    pub events: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MetricGroup {
    pub title: String,
    pub description: String,
    pub metrics: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TopdownMethodologyNode {
    pub name: String,
    pub group: String,
    pub next_items: Vec<String>,
    pub sample_events: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MetricGrouping {
    pub stage_1: Vec<String>,
    pub stage_2: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DecisionTree {
    pub root_nodes: Vec<String>,
    pub metrics: Vec<TopdownMethodologyNode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TopdownMethodology {
    pub title: String,
    pub description: String,
    pub metric_grouping: MetricGrouping,
    pub decision_tree: DecisionTree,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Groups {
    pub function: HashMap<String, FunctionGroup>,
    pub metrics: HashMap<String, MetricGroup>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Methodologies {
    pub topdown_methodology: TopdownMethodology,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TelemetrySpecification {
    pub document: HashMap<String, serde_json::Value>,
    pub product_configuration: ProductConfiguration,
    pub events: HashMap<String, Event>,
    pub metrics: HashMap<String, Metric>,
    pub groups: Groups,
    pub methodologies: Methodologies,
}

impl TelemetrySpecification {
    /// Load and validate a telemetry specification from a JSON file.
    pub fn load_from_file(path: &Path) -> Result<Self, SpecError> {
        let content = std::fs::read_to_string(path).map_err(|e| SpecError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        let spec: TelemetrySpecification =
            serde_json::from_str(&content).map_err(|e| SpecError::Json {
                path: path.display().to_string(),
                source: e,
            })?;
        spec.validate()?;
        Ok(spec)
    }

    /// Load from a JSON string.
    pub fn load_from_str(json: &str) -> Result<Self, SpecError> {
        let spec: TelemetrySpecification =
            serde_json::from_str(json).map_err(|e| SpecError::Json {
                path: "<string>".into(),
                source: e,
            })?;
        spec.validate()?;
        Ok(spec)
    }

    /// Run all validation checks (mirrors the pydantic model_validators).
    pub fn validate(&self) -> Result<(), SpecError> {
        self.validate_metrics_events()?;
        self.validate_function_groups_events()?;
        self.validate_metric_groups_metrics()?;
        self.validate_metric_grouping()?;
        self.validate_decision_tree_root_nodes()?;
        self.validate_decision_tree_metrics()?;
        Ok(())
    }

    fn validate_metrics_events(&self) -> Result<(), SpecError> {
        let mut errors = Vec::new();
        for (m_name, metric) in &self.metrics {
            for ev in &metric.events {
                if !self.events.contains_key(ev) {
                    errors.push(format!(
                        "Metric '{m_name}' references undefined event: '{ev}'"
                    ));
                }
            }
            for ev in &metric.sample_events {
                if !self.events.contains_key(ev) {
                    errors.push(format!(
                        "Metric '{m_name}' references undefined sample_event: '{ev}'"
                    ));
                }
            }
        }
        if !errors.is_empty() {
            return Err(SpecError::Validation(errors.join("; ")));
        }
        Ok(())
    }

    fn validate_function_groups_events(&self) -> Result<(), SpecError> {
        for (fg_name, fg) in &self.groups.function {
            for ev in &fg.events {
                if !self.events.contains_key(ev) {
                    return Err(SpecError::Validation(format!(
                        "Function group '{fg_name}' references undefined event: '{ev}'"
                    )));
                }
            }
        }
        Ok(())
    }

    fn validate_metric_groups_metrics(&self) -> Result<(), SpecError> {
        for (mg_name, mg) in &self.groups.metrics {
            for m in &mg.metrics {
                if !self.metrics.contains_key(m) {
                    return Err(SpecError::Validation(format!(
                        "Metric group '{mg_name}' references undefined metric: '{m}'"
                    )));
                }
            }
        }
        Ok(())
    }

    fn validate_metric_grouping(&self) -> Result<(), SpecError> {
        let mg = &self.methodologies.topdown_methodology.metric_grouping;
        let mut errors = Vec::new();
        for s in &mg.stage_1 {
            if !self.groups.metrics.contains_key(s) {
                errors.push(format!("stage_1 references undefined metric group: '{s}'"));
            }
        }
        for s in &mg.stage_2 {
            if !self.groups.metrics.contains_key(s) {
                errors.push(format!("stage_2 references undefined metric group: '{s}'"));
            }
        }
        let s1: std::collections::HashSet<&str> = mg.stage_1.iter().map(|s| s.as_str()).collect();
        for s in &mg.stage_2 {
            if s1.contains(s.as_str()) {
                errors.push(format!(
                    "Group '{s}' appears in both stage_1 and stage_2"
                ));
            }
        }
        if !errors.is_empty() {
            return Err(SpecError::Validation(errors.join("; ")));
        }
        Ok(())
    }

    fn validate_decision_tree_root_nodes(&self) -> Result<(), SpecError> {
        let dt = &self.methodologies.topdown_methodology.decision_tree;
        for rn in &dt.root_nodes {
            if !self.metrics.contains_key(rn) {
                return Err(SpecError::Validation(format!(
                    "Decision tree root_node references undefined metric: '{rn}'"
                )));
            }
        }
        Ok(())
    }

    fn validate_decision_tree_metrics(&self) -> Result<(), SpecError> {
        let dt = &self.methodologies.topdown_methodology.decision_tree;
        let mut errors = Vec::new();
        for node in &dt.metrics {
            if !self.metrics.contains_key(&node.name) {
                errors.push(format!(
                    "Decision tree node '{}' is not defined in metrics",
                    node.name
                ));
            }
            if !self.groups.metrics.contains_key(&node.group) {
                errors.push(format!(
                    "Decision tree node '{}' has group '{}' not defined in groups.metrics",
                    node.name, node.group
                ));
            } else {
                let group_metrics = &self.groups.metrics[&node.group].metrics;
                if !group_metrics.contains(&node.name) {
                    errors.push(format!(
                        "Decision tree node '{}' is not listed in its group '{}' metrics",
                        node.name, node.group
                    ));
                }
            }
            for item in &node.next_items {
                if !self.metrics.contains_key(item)
                    && !self.groups.metrics.contains_key(item)
                {
                    errors.push(format!(
                        "Decision tree node '{}' has next_item '{}' which is neither a metric nor a metric group",
                        node.name, item
                    ));
                }
            }
        }
        if !errors.is_empty() {
            return Err(SpecError::Validation(errors.join("; ")));
        }
        Ok(())
    }
}

/// CPU ID to spec file mapping (mapping.json).
#[derive(Debug, Clone, Deserialize)]
pub struct CpuMapping {
    pub name: String,
}

/// Load the CPU ID → spec file mapping.
pub fn load_cpu_mapping(path: &Path) -> Result<HashMap<String, CpuMapping>, SpecError> {
    let content = std::fs::read_to_string(path).map_err(|e| SpecError::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    serde_json::from_str(&content).map_err(|e| SpecError::Json {
        path: path.display().to_string(),
        source: e,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hex_str_parsing() {
        let json = r#"{"product_name":"Test","part_num":"0xd4f","implementer":"0x41","major_revision":"0","minor_revision":"0","num_slots":8,"num_bus_slots":0,"architecture":"armv9.0","pmu_architecture":"pmu_v3"}"#;
        let pc: ProductConfiguration = serde_json::from_str(json).unwrap();
        assert_eq!(pc.part_num, "0xd4f");
        assert_eq!(pc.major_revision, 0);
    }
}
