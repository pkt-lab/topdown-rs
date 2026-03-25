// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;
use telemetry_core::database::TelemetryDatabase;
use telemetry_core::spec::TelemetrySpecification;

fn spec_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tools/topdown_tool/topdown_tool/cpu_probe/metrics/neoverse_v2_r0p0_pmu.json")
}

fn load_neoverse_v2() -> Option<(TelemetrySpecification, TelemetryDatabase)> {
    let path = spec_path();
    if !path.exists() {
        eprintln!("Skipping: spec file not found at {}", path.display());
        return None;
    }
    let spec = TelemetrySpecification::load_from_file(&path).unwrap();
    let db = TelemetryDatabase::from_spec(&spec);
    Some((spec, db))
}

#[test]
fn test_load_neoverse_v2_spec() {
    let Some((spec, db)) = load_neoverse_v2() else { return };

    assert_eq!(spec.product_configuration.product_name, "Neoverse V2");
    assert_eq!(spec.product_configuration.part_num, "0xd4f");
    assert_eq!(spec.product_configuration.num_slots, 8);
    assert!(spec.events.len() > 50);
    assert!(spec.metrics.len() > 20);
    assert!(spec.groups.metrics.len() > 5);

    assert_eq!(db.product_name, "Neoverse V2");
    assert_eq!(db.num_slots, 8);
    assert_eq!(db.events.len(), spec.events.len());
    assert_eq!(db.metrics.len(), spec.metrics.len());
}

#[test]
fn test_database_lookups() {
    let Some((_, db)) = load_neoverse_v2() else { return };

    // Test event lookup
    let ev = db.events.get("CPU_CYCLES").unwrap();
    assert_eq!(ev.code, 0x11);
    assert_eq!(ev.perf_name(), "r11");

    // Test case-insensitive group lookup
    assert!(db.find_group("Topdown_L1").is_some());
    assert!(db.find_group("topdown_l1").is_some());
    assert!(db.find_group("TOPDOWNL1").is_some());

    // Test case-insensitive metric lookup
    assert!(db.find_metric("backend_bound").is_some());

    // Test topdown structure
    assert!(!db.topdown.stage_1_group_names.is_empty());
    assert!(!db.topdown.stage_2_group_names.is_empty());
    assert!(!db.topdown.root_metric_names.is_empty());
}

#[test]
fn test_formula_with_real_metrics() {
    let Some((_, db)) = load_neoverse_v2() else { return };

    if let Some(m) = db.metrics.get("backend_bound") {
        let mut vars = std::collections::HashMap::new();
        for ev_name in &m.event_names {
            vars.insert(ev_name.clone(), 100.0);
        }
        let result = telemetry_core::formula::evaluate(&m.formula, &vars);
        assert!(result.is_ok(), "Formula evaluation failed: {:?}", result);
    }
}

#[test]
fn test_event_scheduler_with_real_data() {
    let Some((_, db)) = load_neoverse_v2() else { return };

    let groups: Vec<Vec<Vec<String>>> = db
        .topdown
        .stage_1_group_names
        .iter()
        .filter_map(|gn| db.groups.get(gn))
        .map(|g| {
            g.metric_names
                .iter()
                .filter_map(|mn| db.metrics.get(mn))
                .map(|m| m.event_names.clone())
                .collect()
        })
        .collect();

    let scheduler = telemetry_core::scheduler::EventScheduler::new(
        groups,
        telemetry_core::scheduler::CollectBy::Metric,
        db.num_slots,
    )
    .unwrap();

    assert!(!scheduler.optimized_event_groups.is_empty());
    let chunks = scheduler.get_event_group_chunks(true);
    assert!(!chunks.is_empty());

    for chunk in &chunks {
        let total: std::collections::HashSet<&String> =
            chunk.iter().flat_map(|g| g.iter()).collect();
        assert!(
            total.len() <= db.num_slots,
            "Chunk has {} events but max is {}",
            total.len(),
            db.num_slots
        );
    }
}
