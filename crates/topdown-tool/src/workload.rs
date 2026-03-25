// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 pkt-lab contributors

//! Workload management: command execution, PID monitoring, system-wide capture.
//!
//! Uses direct syscalls — no dependency on external tools like inotifywait.

use crate::perf::{EventConfig, PerfCollector};
use crate::Cli;
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// How to capture: run a command, attach to PIDs, or system-wide.
#[derive(Debug)]
pub enum WorkloadMode {
    Command(Vec<String>),
    Pid(Vec<i32>),
    SystemWide,
}

impl WorkloadMode {
    pub fn from_cli(cli: &Cli) -> Result<Self> {
        if !cli.command.is_empty() {
            return Ok(WorkloadMode::Command(cli.command.clone()));
        }
        if let Some(ref pid_str) = cli.pid {
            let pids: Result<Vec<i32>, _> = pid_str.split(',').map(|s| s.trim().parse()).collect();
            let pids = pids.context("Invalid PID format")?;
            return Ok(WorkloadMode::Pid(pids));
        }
        if cli.system_wide {
            return Ok(WorkloadMode::SystemWide);
        }
        bail!("Specify a command, --pid, or --system-wide (-a)");
    }
}

/// Run a capture session and return aggregated results.
pub fn run_capture(
    mode: &WorkloadMode,
    event_groups: &[Vec<EventConfig>],
    cores: &[i32],
    _interval: Option<u64>,
) -> Result<HashMap<Vec<String>, Vec<Option<f64>>>> {
    match mode {
        WorkloadMode::Command(cmd) => run_command_capture(cmd, event_groups, cores),
        WorkloadMode::Pid(pids) => run_pid_capture(pids, event_groups, cores),
        WorkloadMode::SystemWide => run_systemwide_capture(event_groups, cores),
    }
}

/// Run a command and capture perf events during its execution.
fn run_command_capture(
    command: &[String],
    event_groups: &[Vec<EventConfig>],
    cores: &[i32],
) -> Result<HashMap<Vec<String>, Vec<Option<f64>>>> {
    if command.is_empty() {
        bail!("Empty command");
    }

    // Open perf events (disabled initially)
    let collector = PerfCollector::open(event_groups, cores, None)?;

    // Fork and exec the command
    let mut child = std::process::Command::new(&command[0])
        .args(&command[1..])
        .spawn()
        .with_context(|| format!("Failed to spawn: {}", command[0]))?;

    // Enable counters
    collector.enable()?;

    // Wait for child to complete
    let status = child.wait().context("Failed to wait for child process")?;

    // Disable counters
    collector.disable()?;

    if !status.success() {
        log::warn!("Command exited with status: {status}");
    }

    collector.read_results()
}

/// Attach to existing PIDs and capture until they exit.
fn run_pid_capture(
    pids: &[i32],
    event_groups: &[Vec<EventConfig>],
    _cores: &[i32],
) -> Result<HashMap<Vec<String>, Vec<Option<f64>>>> {
    // Verify PIDs exist
    for &pid in pids {
        let proc_path = format!("/proc/{pid}");
        if !std::path::Path::new(&proc_path).exists() {
            bail!("PID {pid} does not exist");
        }
    }

    // Open one PerfCollector per PID. In PID mode we use core = -1 so the
    // kernel follows the process across all cores automatically.
    let pid_cores: &[i32] = &[-1];
    let mut collectors: Vec<PerfCollector> = Vec::with_capacity(pids.len());
    for &pid in pids {
        let collector = PerfCollector::open(event_groups, pid_cores, Some(pid))
            .with_context(|| format!("Failed to open perf events for PID {pid}"))?;
        collectors.push(collector);
    }

    // Enable counters for all PIDs
    for collector in &collectors {
        collector.enable()?;
    }

    // Wait for all PIDs to exit
    wait_for_pids(pids)?;

    // Disable counters for all PIDs
    for collector in &collectors {
        collector.disable()?;
    }

    // Read and merge results across all PIDs
    let mut merged: HashMap<Vec<String>, Vec<Option<f64>>> = HashMap::new();
    for collector in &collectors {
        let results = collector.read_results()?;
        merge_results(&mut merged, &results);
    }

    Ok(merged)
}

/// Merge per-PID results by summing values for matching event groups.
fn merge_results(
    dest: &mut HashMap<Vec<String>, Vec<Option<f64>>>,
    src: &HashMap<Vec<String>, Vec<Option<f64>>>,
) {
    for (key, src_vals) in src {
        let dest_vals = dest.entry(key.clone()).or_insert_with(|| vec![None; src_vals.len()]);
        for (i, src_val) in src_vals.iter().enumerate() {
            if let Some(sv) = src_val {
                dest_vals[i] = Some(dest_vals[i].unwrap_or(0.0) + sv);
            }
        }
    }
}

/// Wait for PIDs to exit by polling /proc/[pid].
fn wait_for_pids(pids: &[i32]) -> Result<()> {
    use std::thread::sleep;
    use std::time::Duration;

    let mut remaining: Vec<i32> = pids.to_vec();

    while !remaining.is_empty() {
        remaining.retain(|&pid| std::path::Path::new(&format!("/proc/{pid}")).exists());
        if !remaining.is_empty() {
            sleep(Duration::from_millis(100));
        }
    }

    Ok(())
}

/// System-wide capture until Ctrl+C.
fn run_systemwide_capture(
    event_groups: &[Vec<EventConfig>],
    cores: &[i32],
) -> Result<HashMap<Vec<String>, Vec<Option<f64>>>> {
    let collector = PerfCollector::open(event_groups, cores, None)?;

    // Set up Ctrl+C handler
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc_handler(r);

    collector.enable()?;

    log::info!("System-wide capture running. Press Ctrl+C to stop.");

    // Wait for Ctrl+C
    while running.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    collector.disable()?;

    log::info!("Capture stopped.");

    collector.read_results()
}

/// Install a Ctrl+C (SIGINT) handler that sets the flag to false.
fn ctrlc_handler(running: Arc<AtomicBool>) {
    // Use a simple signal handler via libc
    unsafe {
        libc::signal(libc::SIGINT, signal_handler as *const () as libc::sighandler_t);
    }
    // Store the flag in a static for the signal handler
    RUNNING_FLAG.store(running);
}

static RUNNING_FLAG: RunningFlag = RunningFlag::new();

struct RunningFlag {
    inner: std::sync::atomic::AtomicPtr<()>,
}

impl RunningFlag {
    const fn new() -> Self {
        Self {
            inner: std::sync::atomic::AtomicPtr::new(std::ptr::null_mut()),
        }
    }

    fn store(&self, flag: Arc<AtomicBool>) {
        let ptr = Arc::into_raw(flag) as *mut ();
        self.inner.store(ptr, Ordering::SeqCst);
    }

    fn signal(&self) {
        let ptr = self.inner.load(Ordering::SeqCst);
        if !ptr.is_null() {
            let flag = unsafe { &*(ptr as *const AtomicBool) };
            flag.store(false, Ordering::SeqCst);
        }
    }
}

extern "C" fn signal_handler(_sig: libc::c_int) {
    RUNNING_FLAG.signal();
}
