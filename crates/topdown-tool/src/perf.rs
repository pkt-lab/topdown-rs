// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 pkt-lab contributors

//! Direct perf_event_open(2) interface — no `perf` binary dependency.
//!
//! This module uses the Linux perf_event_open syscall to program PMU counters
//! directly, similar to how Android's simpleperf works. This makes the tool
//! fully self-contained on any Linux ≥ 3.2 kernel.

use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::os::unix::io::RawFd;

// ─── perf_event_attr constants ───────────────────────────────────────────────

const PERF_TYPE_RAW: u32 = 4;
const PERF_EVENT_IOC_ENABLE: libc::c_ulong = 0x2400;
const PERF_EVENT_IOC_DISABLE: libc::c_ulong = 0x2401;
const PERF_EVENT_IOC_RESET: libc::c_ulong = 0x2403;

// perf_event_attr flags (bitfield at offset 40)
// bit 0: disabled, bit 1: inherit, bit 2: pinned, bit 3: exclusive,
// bit 4: exclude_user, bit 5: exclude_kernel, bit 6: exclude_hv
const PERF_ATTR_FLAG_DISABLED: u64 = 1 << 0;
const PERF_ATTR_FLAG_EXCLUDE_KERNEL: u64 = 1 << 5;
const PERF_ATTR_FLAG_EXCLUDE_HV: u64 = 1 << 6;

const PERF_FORMAT_TOTAL_TIME_ENABLED: u64 = 1 << 0;
const PERF_FORMAT_TOTAL_TIME_RUNNING: u64 = 1 << 1;

// ─── Types ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct EventConfig {
    pub name: String,
    pub code: u64,
}

struct PerfEventGroup {
    leader_fd: RawFd,
    fds: Vec<RawFd>,
}

impl Drop for PerfEventGroup {
    fn drop(&mut self) {
        for &fd in &self.fds {
            unsafe { libc::close(fd) };
        }
    }
}

// ─── perf_event_open syscall wrapper ─────────────────────────────────────────

/// perf_event_attr as a byte buffer with fields at correct ABI offsets.
/// Avoids struct layout mismatches with the kernel.
const PERF_ATTR_BUF_SIZE: usize = 128;

#[repr(C, align(8))]
#[derive(Clone)]
struct PerfEventAttr {
    buf: [u8; PERF_ATTR_BUF_SIZE],
}

impl PerfEventAttr {
    fn new_raw_event(config: u64, disabled: bool) -> Self {
        let mut attr = Self {
            buf: [0u8; PERF_ATTR_BUF_SIZE],
        };
        // Offsets from linux/perf_event.h (stable ABI)
        attr.set_u32(0, PERF_TYPE_RAW); // type
        attr.set_u32(4, PERF_ATTR_BUF_SIZE as u32); // size
        attr.set_u64(8, config); // config
        attr.set_u64(32, PERF_FORMAT_TOTAL_TIME_ENABLED | PERF_FORMAT_TOTAL_TIME_RUNNING);

        let mut flags: u64 = 0;
        if disabled {
            flags |= PERF_ATTR_FLAG_DISABLED;
        }
        flags |= PERF_ATTR_FLAG_EXCLUDE_KERNEL;
        flags |= PERF_ATTR_FLAG_EXCLUDE_HV;
        attr.set_u64(40, flags); // flags bitfield

        attr
    }

    fn set_u32(&mut self, offset: usize, val: u32) {
        self.buf[offset..offset + 4].copy_from_slice(&val.to_ne_bytes());
    }

    fn set_u64(&mut self, offset: usize, val: u64) {
        self.buf[offset..offset + 8].copy_from_slice(&val.to_ne_bytes());
    }
}

unsafe fn perf_event_open(
    attr: &PerfEventAttr,
    pid: i32,
    cpu: i32,
    group_fd: i32,
    flags: u64,
) -> RawFd {
    libc::syscall(
        libc::SYS_perf_event_open,
        attr.buf.as_ptr(),
        pid,
        cpu,
        group_fd,
        flags,
    ) as RawFd
}

// ─── Public API ──────────────────────────────────────────────────────────────

/// Check if the current process has permission to use perf_event_open.
pub fn check_perf_privilege() -> Result<()> {
    // Check perf_event_paranoid
    let paranoid = std::fs::read_to_string("/proc/sys/kernel/perf_event_paranoid")
        .unwrap_or_else(|_| "2".into())
        .trim()
        .parse::<i32>()
        .unwrap_or(2);

    if paranoid <= 1 {
        return Ok(());
    }

    // Check if running as root
    if unsafe { libc::geteuid() } == 0 {
        return Ok(());
    }

    // Check CAP_PERFMON (bit 38) or CAP_SYS_ADMIN (bit 21)
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if let Some(hex) = line.strip_prefix("CapEff:\t") {
                if let Ok(caps) = u64::from_str_radix(hex.trim(), 16) {
                    let cap_perfmon = 1u64 << 38;
                    let cap_sys_admin = 1u64 << 21;
                    if caps & (cap_perfmon | cap_sys_admin) != 0 {
                        return Ok(());
                    }
                }
            }
        }
    }

    bail!(
        "perf_event_paranoid is {paranoid} and process lacks CAP_PERFMON/CAP_SYS_ADMIN. \
         Set /proc/sys/kernel/perf_event_paranoid to -1 or run as root."
    );
}

/// Detect the number of available PMU counters on a given core via binary search.
pub fn detect_pmu_counters(core: i32) -> Result<usize> {
    let mut low = 1usize;
    let mut high = 32usize;
    let mut last_good = 1usize;

    while low <= high {
        let mid = (low + high) / 2;
        if can_open_n_events(mid, core) {
            last_good = mid;
            low = mid + 1;
        } else {
            if mid == 0 {
                break;
            }
            high = mid - 1;
        }
    }

    Ok(last_good)
}

/// Well-known Arm architecture-defined PMU events (guaranteed on all v8+ CPUs).
const ARCH_EVENTS: &[u64] = &[
    0x08, // INST_RETIRED
    0x11, // CPU_CYCLES
    0x1B, // INST_SPEC
    0x01, // L1I_CACHE_REFILL
    0x03, // L1D_CACHE_REFILL
    0x04, // L1D_CACHE
    0x06, // MEM_ACCESS
    0x10, // BR_MIS_PRED
    0x12, // BR_PRED
    0x13, // MEM_ACCESS
    0x14, // L1I_CACHE
    0x15, // L1D_CACHE_WB
    0x16, // L2D_CACHE
    0x17, // L2D_CACHE_REFILL
    0x18, // L2D_CACHE_WB
    0x19, // BUS_ACCESS
    0x1D, // BUS_CYCLES
    0x00, // SW_INCR
    0x02, // L1I_TLB_REFILL
    0x05, // L1D_TLB_REFILL
    0x09, // EXC_TAKEN
    0x0A, // EXC_RETURN
    0x0C, // BR_RETIRED (PC_WRITE_RETIRED)
    0x0D, // BR_IMMED_RETIRED (SW_INCR on older)
    0x1A, // MEMORY_ERROR
    0x1E, // CHAIN
    0x21, // BR_RETIRED
    0x22, // BR_MIS_PRED_RETIRED
    0x23, // STALL_FRONTEND
    0x24, // STALL_BACKEND
    0x25, // L1D_TLB
    0x26, // L1I_TLB
];

fn can_open_n_events(n: usize, core: i32) -> bool {
    let mut fds: Vec<RawFd> = Vec::new();

    for i in 0..n {
        let group_fd = if i == 0 { -1 } else { fds[0] };
        let disabled = i == 0;

        // Use different well-known architecture events to avoid duplicates
        let event_code = ARCH_EVENTS[i % ARCH_EVENTS.len()];
        // If we need more events than ARCH_EVENTS, add an offset to avoid
        // exact duplicates in the same group (PMU typically rejects that)
        let event_code = if i >= ARCH_EVENTS.len() {
            event_code | 0x8000 // Use implementation-defined space
        } else {
            event_code
        };

        let attr = PerfEventAttr::new_raw_event(event_code, disabled);

        let fd = unsafe { perf_event_open(&attr, -1, core, group_fd, 0) };
        if fd < 0 {
            for &f in &fds {
                unsafe { libc::close(f) };
            }
            return false;
        }
        fds.push(fd);
    }

    for &f in &fds {
        unsafe { libc::close(f) };
    }
    true
}

/// Managed perf event collection handle for workload capture.
pub struct PerfCollector {
    groups: Vec<PerfEventGroup>,
    /// Event names per group (for result key generation).
    event_names: Vec<Vec<String>>,
    cores_count: usize,
}

impl PerfCollector {
    pub fn open(
        event_groups: &[Vec<EventConfig>],
        cores: &[i32],
        pid: Option<i32>,
    ) -> Result<Self> {
        let mut groups = Vec::new();
        let target_pid = pid.unwrap_or(-1);

        for event_group in event_groups {
            for &core in cores {
                let pg = open_event_group(event_group, core, Some(target_pid))
                    .with_context(|| {
                        format!(
                            "Failed to open perf events on core {core}: {:?}",
                            event_group.iter().map(|e| &e.name).collect::<Vec<_>>()
                        )
                    })?;
                groups.push(pg);
            }
        }

        let event_names: Vec<Vec<String>> = event_groups
            .iter()
            .map(|eg| eg.iter().map(|e| e.name.clone()).collect())
            .collect();

        Ok(Self {
            groups,
            event_names,
            cores_count: cores.len(),
        })
    }

    pub fn enable(&self) -> Result<()> {
        for g in &self.groups {
            enable_group(g)?;
        }
        Ok(())
    }

    pub fn disable(&self) -> Result<()> {
        for g in &self.groups {
            disable_group(g)?;
        }
        Ok(())
    }

    pub fn read_results(&self) -> Result<HashMap<Vec<String>, Vec<Option<f64>>>> {
        let mut results = HashMap::new();

        for (eg_idx, names) in self.event_names.iter().enumerate() {
            let n = names.len();
            let mut aggregated = vec![0.0f64; n];
            let mut valid = vec![true; n];

            for core_idx in 0..self.cores_count {
                let pg = &self.groups[eg_idx * self.cores_count + core_idx];
                let values = read_group_values(pg)?;
                aggregate_values(&mut aggregated, &mut valid, &values);
            }

            results.insert(names.clone(), finalize_values(&aggregated, &valid));
        }

        Ok(results)
    }
}

fn aggregate_values(dest: &mut [f64], valid: &mut [bool], src: &[Option<f64>]) {
    for (i, val) in src.iter().enumerate() {
        if let Some(v) = val {
            dest[i] += v;
        } else {
            valid[i] = false;
        }
    }
}

fn finalize_values(vals: &[f64], valid: &[bool]) -> Vec<Option<f64>> {
    vals.iter()
        .zip(valid.iter())
        .map(|(&v, &ok)| if ok { Some(v) } else { None })
        .collect()
}

// ─── Internal helpers ────────────────────────────────────────────────────────

fn open_event_group(
    events: &[EventConfig],
    cpu: i32,
    pid: Option<i32>,
) -> Result<PerfEventGroup> {
    let target_pid = pid.unwrap_or(-1);
    let mut fds = Vec::with_capacity(events.len());

    for (i, ev) in events.iter().enumerate() {
        let group_fd = if i == 0 { -1 } else { fds[0] };
        let disabled = i == 0;

        let attr = PerfEventAttr::new_raw_event(ev.code, disabled);
        let fd = unsafe { perf_event_open(&attr, target_pid, cpu, group_fd, 0) };

        if fd < 0 {
            let errno = std::io::Error::last_os_error();
            for &f in &fds {
                unsafe { libc::close(f) };
            }
            bail!(
                "perf_event_open failed for event '{}' (code 0x{:x}) on cpu {cpu}: {errno}",
                ev.name,
                ev.code
            );
        }

        fds.push(fd);
    }

    Ok(PerfEventGroup {
        leader_fd: fds[0],
        fds,
    })
}

fn enable_group(group: &PerfEventGroup) -> Result<()> {
    let ret = unsafe { libc::ioctl(group.leader_fd, PERF_EVENT_IOC_RESET as _, 0) };
    if ret < 0 {
        bail!(
            "ioctl PERF_EVENT_IOC_RESET failed: {}",
            std::io::Error::last_os_error()
        );
    }
    let ret = unsafe { libc::ioctl(group.leader_fd, PERF_EVENT_IOC_ENABLE as _, 0) };
    if ret < 0 {
        bail!(
            "ioctl PERF_EVENT_IOC_ENABLE failed: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

fn disable_group(group: &PerfEventGroup) -> Result<()> {
    let ret = unsafe { libc::ioctl(group.leader_fd, PERF_EVENT_IOC_DISABLE as _, 0) };
    if ret < 0 {
        bail!(
            "ioctl PERF_EVENT_IOC_DISABLE failed: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

/// Read counter values from each fd individually.
/// Returns value for each event, scaled by time_enabled/time_running for multiplexing.
fn read_group_values(group: &PerfEventGroup) -> Result<Vec<Option<f64>>> {
    let mut results = Vec::with_capacity(group.fds.len());

    for &fd in &group.fds {
        let mut buf = [0u8; 24]; // value (u64), time_enabled (u64), time_running (u64)
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, 24) };

        if n < 24 {
            results.push(None);
            continue;
        }

        let value = u64::from_ne_bytes(buf[0..8].try_into().unwrap());
        let time_enabled = u64::from_ne_bytes(buf[8..16].try_into().unwrap());
        let time_running = u64::from_ne_bytes(buf[16..24].try_into().unwrap());

        if time_running == 0 || time_enabled == 0 {
            results.push(Some(0.0));
        } else if time_running < time_enabled {
            // Multiplexing — scale the value
            let scaled = (value as f64) * (time_enabled as f64) / (time_running as f64);
            results.push(Some(scaled));
        } else {
            results.push(Some(value as f64));
        }
    }

    Ok(results)
}

/// Read the MIDR_EL1 register value for a given CPU core from sysfs.
pub fn read_midr(core: i32) -> Result<u64> {
    let path = format!(
        "/sys/devices/system/cpu/cpu{core}/regs/identification/midr_el1"
    );
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read MIDR from {path}"))?;
    let trimmed = content.trim();
    let stripped = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    u64::from_str_radix(stripped, 16).with_context(|| format!("Invalid MIDR value: {trimmed}"))
}

/// Extract CPU ID (implementer << 12 | part_num) from MIDR value.
pub fn cpu_id_from_midr(midr: u64) -> u64 {
    let implementer = (midr >> 24) & 0xFF;
    let part_num = (midr >> 4) & 0xFFF;
    (implementer << 12) | part_num
}
