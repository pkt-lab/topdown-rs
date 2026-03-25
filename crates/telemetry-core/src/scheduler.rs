// SPDX-License-Identifier: Apache-2.0
// Copyright 2022-2025 Arm Limited (original Python implementation)
// Copyright 2026 pkt-lab contributors (Rust reimplementation)

//! Event scheduling and PMU counter bin-packing.
//!
//! Ported from `event_scheduler.py`. Schedules event groups into chunks that
//! fit within the hardware PMU counter limit, and provides result retrieval
//! to map collected data back to the original metric/group structure.

use std::collections::HashSet;
use std::fmt;
use thiserror::Error;

// ─── CollectBy strategy ──────────────────────────────────────────────────────

/// How events are grouped for collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectBy {
    /// Each event is collected independently.
    None,
    /// Each metric's events are collected as a group.
    Metric,
    /// Overlapping event groups across metrics are merged.
    Group,
}

impl fmt::Display for CollectBy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CollectBy::None => write!(f, "none"),
            CollectBy::Metric => write!(f, "metric"),
            CollectBy::Group => write!(f, "group"),
        }
    }
}

impl CollectBy {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "none" => Some(CollectBy::None),
            "metric" => Some(CollectBy::Metric),
            "group" => Some(CollectBy::Group),
            _ => Option::None,
        }
    }
}

// ─── Errors ──────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
#[error("event group requires {required} unique events but only {available} PMU counters available")]
pub struct GroupScheduleError {
    pub required: usize,
    pub available: usize,
    pub event_names: Vec<String>,
}

// ─── EventScheduler ──────────────────────────────────────────────────────────

/// Schedules event groups for PMU collection, respecting counter limits.
///
/// Generic over event name type for flexibility, but in practice uses `String`.
///
/// # Type Parameters
/// - `groups`: For each metric group, a list of event tuples (one per metric).
///   Each event tuple is the set of event names needed by that metric.
pub struct EventScheduler {
    max_events: usize,
    pub optimized_event_groups: Vec<Vec<String>>,
}

impl EventScheduler {
    /// Create a new scheduler.
    ///
    /// # Arguments
    /// - `groups`: For each metric group, a list of event-name tuples (one per metric).
    /// - `collect_by`: Collection strategy.
    /// - `max_events`: Maximum PMU counter slots available.
    pub fn new(
        groups: Vec<Vec<Vec<String>>>,
        collect_by: CollectBy,
        max_events: usize,
    ) -> Result<Self, GroupScheduleError> {
        let event_list = generate_event_list(&groups, collect_by)?;
        let optimized = optimize_event_groups(event_list);

        // Validate no single optimized group exceeds max_events
        for eg in &optimized {
            if eg.len() > max_events {
                return Err(GroupScheduleError {
                    required: eg.len(),
                    available: max_events,
                    event_names: eg.clone(),
                });
            }
        }

        Ok(Self {
            max_events,
            optimized_event_groups: optimized,
        })
    }

    /// Get an iterator that yields chunks of event groups fitting within max_events.
    pub fn get_event_group_chunks(&self, split: bool) -> Vec<Vec<Vec<String>>> {
        if !split {
            return vec![self.optimized_event_groups.clone()];
        }
        make_chunks(&self.optimized_event_groups, self.max_events)
    }

}

// ─── Internal helpers ────────────────────────────────────────────────────────

/// Generate the event list based on the collection strategy.
fn generate_event_list(
    groups: &[Vec<Vec<String>>],
    collect_by: CollectBy,
) -> Result<Vec<Vec<String>>, GroupScheduleError> {
    match collect_by {
        CollectBy::None => {
            // Each individual event is its own group
            let mut all_events = HashSet::new();
            for group in groups {
                for metric_events in group {
                    for ev in metric_events {
                        all_events.insert(ev.clone());
                    }
                }
            }
            let mut result: Vec<Vec<String>> = all_events.into_iter().map(|e| vec![e]).collect();
            result.sort();
            Ok(result)
        }
        CollectBy::Metric => {
            // Each metric's events form a group
            let mut result = Vec::new();
            for group in groups {
                for metric_events in group {
                    if !metric_events.is_empty() {
                        let mut sorted = metric_events.clone();
                        sorted.sort();
                        sorted.dedup();
                        result.push(sorted);
                    }
                }
            }
            Ok(result)
        }
        CollectBy::Group => {
            // All events in a metric group are merged
            let mut result = Vec::new();
            for group in groups {
                let mut all: Vec<String> = group.iter().flatten().cloned().collect();
                all.sort();
                all.dedup();
                if !all.is_empty() {
                    result.push(all);
                }
            }
            Ok(result)
        }
    }
}

/// Remove duplicate event groups and merge groups that are subsets of others.
fn optimize_event_groups(mut groups: Vec<Vec<String>>) -> Vec<Vec<String>> {
    // Sort each group internally
    for g in &mut groups {
        g.sort();
        g.dedup();
    }

    // Remove exact duplicates
    groups.sort();
    groups.dedup();

    // Remove groups that are subsets of other groups
    let mut optimized = Vec::new();
    for (i, group) in groups.iter().enumerate() {
        let is_subset = groups.iter().enumerate().any(|(j, other)| {
            i != j && other.len() >= group.len() && group.iter().all(|e| other.contains(e))
        });
        if !is_subset {
            optimized.push(group.clone());
        }
    }

    optimized
}

/// Split event groups into chunks where total unique events per chunk ≤ max_events.
fn make_chunks(event_groups: &[Vec<String>], max_events: usize) -> Vec<Vec<Vec<String>>> {
    let mut chunks: Vec<Vec<Vec<String>>> = Vec::new();
    let mut current_chunk: Vec<Vec<String>> = Vec::new();
    let mut current_events: HashSet<String> = HashSet::new();

    for group in event_groups {
        let new_events: HashSet<&String> = group.iter().collect();
        let would_add: usize = new_events
            .iter()
            .filter(|e| !current_events.contains(**e))
            .count();

        if !current_chunk.is_empty() && current_events.len() + would_add > max_events {
            chunks.push(std::mem::take(&mut current_chunk));
            current_events.clear();
        }

        for ev in group {
            current_events.insert(ev.clone());
        }
        current_chunk.push(group.clone());
    }

    if !current_chunk.is_empty() {
        chunks.push(current_chunk);
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_collect_by_none() {
        let groups = vec![vec![
            vec!["A".into(), "B".into()],
            vec!["B".into(), "C".into()],
        ]];
        let scheduler = EventScheduler::new(groups, CollectBy::None, 6).unwrap();
        // Each event is its own group
        assert_eq!(scheduler.optimized_event_groups.len(), 3);
    }

    #[test]
    fn test_collect_by_metric() {
        let groups = vec![vec![
            vec!["A".into(), "B".into()],
            vec!["B".into(), "C".into()],
        ]];
        let scheduler = EventScheduler::new(groups, CollectBy::Metric, 6).unwrap();
        assert_eq!(scheduler.optimized_event_groups.len(), 2);
    }

    #[test]
    fn test_collect_by_group() {
        let groups = vec![vec![
            vec!["A".into(), "B".into()],
            vec!["B".into(), "C".into()],
        ]];
        let scheduler = EventScheduler::new(groups, CollectBy::Group, 6).unwrap();
        // All events merged into one group: [A, B, C]
        assert_eq!(scheduler.optimized_event_groups.len(), 1);
        assert_eq!(scheduler.optimized_event_groups[0].len(), 3);
    }

    #[test]
    fn test_chunking() {
        let groups = vec![
            vec!["A".into(), "B".into()],
            vec!["C".into(), "D".into()],
            vec!["E".into(), "F".into()],
        ];
        let chunks = make_chunks(&groups, 4);
        // First chunk: A,B,C,D (4 events), second chunk: E,F (2 events)
        assert_eq!(chunks.len(), 2);
    }

    #[test]
    fn test_optimize_removes_subsets() {
        let groups = vec![
            vec!["A".into(), "B".into()],
            vec!["A".into(), "B".into(), "C".into()],
        ];
        let optimized = optimize_event_groups(groups);
        assert_eq!(optimized.len(), 1);
        assert_eq!(optimized[0], vec!["A", "B", "C"]);
    }

    #[test]
    fn test_group_too_large() {
        let groups = vec![vec![vec![
            "A".into(),
            "B".into(),
            "C".into(),
            "D".into(),
        ]]];
        let result = EventScheduler::new(groups, CollectBy::Group, 2);
        assert!(result.is_err());
    }
}
