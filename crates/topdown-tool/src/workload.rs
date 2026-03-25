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
use std::time::{Duration, Instant};

/// A single timestamped snapshot of event counter values.
#[derive(Debug, Clone)]
pub struct TimestampedSnapshot {
    /// Seconds elapsed since capture start.
    pub timestamp: f64,
    /// Event counter values for this interval.
    pub results: HashMap<Vec<String>, Vec<Option<f64>>>,
}

/// Result of a capture session — either a single aggregated result
/// or a series of timestamped interval snapshots.
#[derive(Debug)]
pub enum CaptureResult {
    /// Single aggregated result (non-interval mode).
    Aggregated(HashMap<Vec<String>, Vec<Option<f64>>>),
    /// Time-series of interval snapshots.
    Intervals(Vec<TimestampedSnapshot>),
}

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

/// Run a capture session and return results.
pub fn run_capture(
    mode: &WorkloadMode,
    event_groups: &[Vec<EventConfig>],
    cores: &[i32],
    interval: Option<u64>,
) -> Result<CaptureResult> {
    match mode {
        WorkloadMode::Command(cmd) => run_command_capture(cmd, event_groups, cores, interval),
        WorkloadMode::Pid(pids) => run_pid_capture(pids, event_groups, cores, interval),
        WorkloadMode::SystemWide => run_systemwide_capture(event_groups, cores, interval),
    }
}

/// Run a command and capture perf events during its execution.
fn run_command_capture(
    command: &[String],
    event_groups: &[Vec<EventConfig>],
    cores: &[i32],
    interval: Option<u64>,
) -> Result<CaptureResult> {
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

    if let Some(interval_ms) = interval {
        // Interval mode: periodically read and reset counters
        let interval_dur = Duration::from_millis(interval_ms);
        let start = Instant::now();
        let mut snapshots = Vec::new();

        loop {
            std::thread::sleep(interval_dur);

            // Check if child has exited
            match child.try_wait() {
                Ok(Some(status)) => {
                    // Child exited — do one final read
                    let results = collector.read_results()?;
                    let elapsed = start.elapsed().as_secs_f64();
                    snapshots.push(TimestampedSnapshot {
                        timestamp: elapsed,
                        results,
                    });
                    collector.disable()?;
                    if !status.success() {
                        log::warn!("Command exited with status: {status}");
                    }
                    break;
                }
                Ok(None) => {
                    // Still running — read and reset
                    let results = collector.read_and_reset()?;
                    let elapsed = start.elapsed().as_secs_f64();
                    snapshots.push(TimestampedSnapshot {
                        timestamp: elapsed,
                        results,
                    });
                }
                Err(e) => {
                    collector.disable()?;
                    return Err(e).context("Failed to check child status");
                }
            }
        }

        Ok(CaptureResult::Intervals(snapshots))
    } else {
        // Non-interval mode: wait for child to complete
        let status = child.wait().context("Failed to wait for child process")?;
        collector.disable()?;

        if !status.success() {
            log::warn!("Command exited with status: {status}");
        }

        Ok(CaptureResult::Aggregated(collector.read_results()?))
    }
}

/// Attach to existing PIDs and capture until they exit.
fn run_pid_capture(
    pids: &[i32],
    event_groups: &[Vec<EventConfig>],
    cores: &[i32],
    interval: Option<u64>,
) -> Result<CaptureResult> {
    // Verify PIDs exist
    for &pid in pids {
        let proc_path = format!("/proc/{pid}");
        if !std::path::Path::new(&proc_path).exists() {
            bail!("PID {pid} does not exist");
        }
    }

    // Open perf events targeting the first PID (or system-wide with core filter)
    // For PID mode, we open per-pid events
    let collector = PerfCollector::open(event_groups, cores, Some(pids[0]))?;

    // Enable counters
    collector.enable()?;

    if let Some(interval_ms) = interval {
        let interval_dur = Duration::from_millis(interval_ms);
        let start = Instant::now();
        let mut snapshots = Vec::new();
        let mut remaining: Vec<i32> = pids.to_vec();

        while !remaining.is_empty() {
            std::thread::sleep(interval_dur);

            let results = collector.read_and_reset()?;
            let elapsed = start.elapsed().as_secs_f64();
            snapshots.push(TimestampedSnapshot {
                timestamp: elapsed,
                results,
            });

            remaining.retain(|&pid| std::path::Path::new(&format!("/proc/{pid}")).exists());
        }

        collector.disable()?;
        Ok(CaptureResult::Intervals(snapshots))
    } else {
        // Wait for PIDs to exit using polling on /proc/[pid]
        wait_for_pids(pids)?;
        collector.disable()?;
        Ok(CaptureResult::Aggregated(collector.read_results()?))
    }
}

/// Wait for PIDs to exit by polling /proc/[pid].
fn wait_for_pids(pids: &[i32]) -> Result<()> {
    use std::thread::sleep;

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
    interval: Option<u64>,
) -> Result<CaptureResult> {
    let collector = PerfCollector::open(event_groups, cores, None)?;

    // Set up Ctrl+C handler
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc_handler(r);

    collector.enable()?;

    log::info!("System-wide capture running. Press Ctrl+C to stop.");

    if let Some(interval_ms) = interval {
        let interval_dur = Duration::from_millis(interval_ms);
        let start = Instant::now();
        let mut snapshots = Vec::new();

        while running.load(Ordering::SeqCst) {
            std::thread::sleep(interval_dur);
            if !running.load(Ordering::SeqCst) {
                break;
            }

            let results = collector.read_and_reset()?;
            let elapsed = start.elapsed().as_secs_f64();
            snapshots.push(TimestampedSnapshot {
                timestamp: elapsed,
                results,
            });
        }

        collector.disable()?;
        log::info!("Capture stopped.");
        Ok(CaptureResult::Intervals(snapshots))
    } else {
        // Wait for Ctrl+C
        while running.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(100));
        }

        collector.disable()?;
        log::info!("Capture stopped.");
        Ok(CaptureResult::Aggregated(collector.read_results()?))
    }
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
