//! Pure collection protocol and parser/differencing kernel for `host_resource_v1`.
//!
//! This module deliberately does not know about SSH, HTTP, storage, scheduling, or
//! credentials.  The integration layer executes [`host_resource_v1_profile`] as
//! fixed actions and maps each result into [`HostResourceCapture`].  No command in
//! the profile accepts user input.

use std::collections::BTreeMap;

pub const HOST_RESOURCE_PROFILE_ID: &str = "host_resource_v1";
pub const HOST_RESOURCE_PROTOCOL_VERSION: u32 = 1;
pub const COUNTER_SAMPLE_DELAY_MILLIS: u64 = 1_000;
pub const MIN_COUNTER_INTERVAL_SECONDS: f64 = 0.5;
pub const MAX_FILESYSTEMS: usize = 256;
pub const MAX_BLOCK_DEVICES: usize = 512;
pub const MAX_INTERFACES: usize = 512;
pub const MAX_PROCESS_ENTRIES: usize = 4_096;
const FILESYSTEM_ACTION_TIMEOUT_SECONDS: u8 = 3;
const PROCESS_ACTION_TIMEOUT_SECONDS: u8 = 2;

const BATCH_BEGIN_MARKER: &str = "__NETWORK_ATLAS_HOST_RESOURCE_V1_BEGIN__";
const BATCH_END_MARKER: &str = "__NETWORK_ATLAS_HOST_RESOURCE_V1_END__";

const BOOT_UPTIME_COMMAND: &str =
    "LC_ALL=C sh -c 'cat /proc/sys/kernel/random/boot_id; cat /proc/uptime'";
const CPU_STAT_COMMAND: &str = "LC_ALL=C cat /proc/stat";
const DISK_IO_COMMAND: &str = "LC_ALL=C cat /proc/diskstats";
const NETWORK_COMMAND: &str = "LC_ALL=C cat /proc/net/dev";
const NETWORK_IDENTITY_COMMAND: &str = "LC_ALL=C sh -c 'for path in /sys/class/net/*; do [ -e \"$path\" ] || continue; name=${path##*/}; ifindex=-; iflink=-; operstate=unknown; speed=-; IFS= read -r ifindex < \"$path/ifindex\" || continue; IFS= read -r iflink < \"$path/iflink\" || iflink=-; IFS= read -r operstate < \"$path/operstate\" || operstate=unknown; IFS= read -r speed < \"$path/speed\" || speed=-; printf \"%s\\t%s\\t%s\\t%s\\t%s\\n\" \"$name\" \"$ifindex\" \"$iflink\" \"$operstate\" \"$speed\"; done'";
const CPU_ONLINE_COMMAND: &str = "LC_ALL=C sh -c 'if [ -r /sys/devices/system/cpu/online ]; then printf \"list \"; cat /sys/devices/system/cpu/online; elif command -v getconf >/dev/null 2>&1; then printf \"count \"; getconf _NPROCESSORS_ONLN; else exit 127; fi'";
const MEMORY_COMMAND: &str = "LC_ALL=C cat /proc/meminfo";
const LOAD_COMMAND: &str = "LC_ALL=C cat /proc/loadavg";
const DISK_CAPACITY_COMMAND: &str = "LC_ALL=C df -P -k";
const DISK_INODE_COMMAND: &str = "LC_ALL=C df -P -i";
const PROCESS_SUMMARY_COMMAND: &str = "LC_ALL=C sh -c 'limit=4096; scanned=0; running=0; blocked=0; zombie=0; raced=0; truncated=0; for path in /proc/[0-9]*; do if [ \"$scanned\" -ge \"$limit\" ]; then truncated=1; break; fi; if ! IFS= read -r line < \"$path/stat\"; then raced=$((raced+1)); continue; fi; rest=${line##*) }; state=${rest%% *}; case \"$state\" in R) running=$((running+1));; D) blocked=$((blocked+1));; Z) zombie=$((zombie+1));; esac; scanned=$((scanned+1)); done; printf \"scanned=%s running=%s blocked=%s zombie=%s raced=%s truncated=%s\\n\" \"$scanned\" \"$running\" \"$blocked\" \"$zombie\" \"$raced\" \"$truncated\"'";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MetricFamily {
    Cpu,
    Memory,
    Load,
    DiskCapacity,
    DiskIo,
    Network,
    Uptime,
    Process,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionPhase {
    CounterFrameA,
    CounterFrameB,
    Snapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedAction {
    pub action_id: &'static str,
    pub family: MetricFamily,
    pub phase: CollectionPhase,
    pub command: &'static str,
    pub max_stdout_bytes: usize,
    pub max_items: usize,
    pub required_for_run: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileStep {
    Action(FixedAction),
    LocalDelayMillis(u64),
}

/// Returns the complete fixed protocol.  The caller must execute actions in
/// order and implement `LocalDelayMillis` locally rather than by constructing a
/// remote shell string.
pub fn host_resource_v1_profile() -> Vec<ProfileStep> {
    use CollectionPhase::{CounterFrameA as A, CounterFrameB as B, Snapshot as S};
    use MetricFamily::{Cpu, DiskCapacity, DiskIo, Load, Memory, Network, Process, Uptime};

    vec![
        action(
            "frame_a_boot_uptime",
            Uptime,
            A,
            BOOT_UPTIME_COMMAND,
            4 * 1024,
            2,
            true,
        ),
        action(
            "frame_a_cpu",
            Cpu,
            A,
            CPU_STAT_COMMAND,
            256 * 1024,
            4_096,
            true,
        ),
        action(
            "frame_a_disk_io",
            DiskIo,
            A,
            DISK_IO_COMMAND,
            256 * 1024,
            MAX_BLOCK_DEVICES,
            false,
        ),
        action(
            "frame_a_network",
            Network,
            A,
            NETWORK_COMMAND,
            256 * 1024,
            MAX_INTERFACES,
            false,
        ),
        action(
            "frame_a_network_identity",
            Network,
            A,
            NETWORK_IDENTITY_COMMAND,
            128 * 1024,
            MAX_INTERFACES,
            false,
        ),
        ProfileStep::LocalDelayMillis(COUNTER_SAMPLE_DELAY_MILLIS),
        action(
            "frame_b_boot_uptime",
            Uptime,
            B,
            BOOT_UPTIME_COMMAND,
            4 * 1024,
            2,
            true,
        ),
        action(
            "frame_b_cpu",
            Cpu,
            B,
            CPU_STAT_COMMAND,
            256 * 1024,
            4_096,
            true,
        ),
        action(
            "frame_b_disk_io",
            DiskIo,
            B,
            DISK_IO_COMMAND,
            256 * 1024,
            MAX_BLOCK_DEVICES,
            false,
        ),
        action(
            "frame_b_network",
            Network,
            B,
            NETWORK_COMMAND,
            256 * 1024,
            MAX_INTERFACES,
            false,
        ),
        action(
            "frame_b_network_identity",
            Network,
            B,
            NETWORK_IDENTITY_COMMAND,
            128 * 1024,
            MAX_INTERFACES,
            false,
        ),
        action("cpu_online", Cpu, S, CPU_ONLINE_COMMAND, 4 * 1024, 1, true),
        action("memory", Memory, S, MEMORY_COMMAND, 64 * 1024, 128, true),
        action("load", Load, S, LOAD_COMMAND, 4 * 1024, 1, true),
        action(
            "disk_capacity",
            DiskCapacity,
            S,
            DISK_CAPACITY_COMMAND,
            256 * 1024,
            MAX_FILESYSTEMS,
            true,
        ),
        action(
            "disk_inodes",
            DiskCapacity,
            S,
            DISK_INODE_COMMAND,
            256 * 1024,
            MAX_FILESYSTEMS,
            false,
        ),
        action(
            "process_summary",
            Process,
            S,
            PROCESS_SUMMARY_COMMAND,
            4 * 1024,
            MAX_PROCESS_ENTRIES,
            false,
        ),
    ]
}

/// Builds the one and only remote command used by the H1 integration. Every
/// fragment comes from [`host_resource_v1_profile`]; no caller supplied value
/// is interpolated. Running the whole profile through one remote shell keeps a
/// manual snapshot to one SSH authentication while retaining per-action exit
/// codes and bounded parsers.
pub fn host_resource_v1_batch_command() -> String {
    let mut script = String::from("set +e\n");
    for step in host_resource_v1_profile() {
        match step {
            ProfileStep::Action(action) => {
                let command = match action.action_id {
                    "disk_capacity" | "disk_inodes" => format!(
                        "if command -v timeout >/dev/null 2>&1; then timeout {FILESYSTEM_ACTION_TIMEOUT_SECONDS} sh -c {}; else exit 127; fi",
                        shell_single_quote(action.command),
                    ),
                    "process_summary" => format!(
                        "if command -v timeout >/dev/null 2>&1; then timeout {PROCESS_ACTION_TIMEOUT_SECONDS} sh -c {}; else exit 127; fi",
                        shell_single_quote(action.command),
                    ),
                    _ => action.command.to_owned(),
                };
                script.push_str(&format!(
                    "printf '%s\\n' '{}{}'\nexec 3>&1\nnetwork_atlas_error=$( ( {} ) 2>&1 1>&3 )\nnetwork_atlas_rc=$?\nexec 3>&-\ncase \"$network_atlas_error\" in *[Pp]ermission*[Dd]enied*) network_atlas_rc=126 ;; esac\nprintf '\\n%s:%s\\n' '{}{}' \"$network_atlas_rc\"\n",
                    BATCH_BEGIN_MARKER,
                    action.action_id,
                    command,
                    BATCH_END_MARKER,
                    action.action_id,
                ));
            }
            ProfileStep::LocalDelayMillis(delay) => {
                // The profile owns this compile-time value. Millisecond input
                // is converted without accepting a duration from HTTP/Agent.
                let seconds = delay as f64 / 1_000.0;
                script.push_str(&format!("sleep {seconds:.3}\n"));
            }
        }
    }
    format!("LC_ALL=C sh -c {}", shell_single_quote(&script))
}

/// Splits a successful batch command into the same typed capture consumed by
/// the pure parser. A non-zero action is represented as a quality value; it
/// never erases outputs from other metric families.
pub fn parse_host_resource_v1_batch(stdout: &str) -> Result<HostResourceCapture, &'static str> {
    let mut capture = HostResourceCapture::default();
    let mut seen = std::collections::BTreeSet::new();
    let mut cursor = 0usize;
    while cursor < stdout.len() {
        while cursor < stdout.len() && stdout.as_bytes()[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor == stdout.len() {
            break;
        }
        let begin = stdout[cursor..]
            .strip_prefix(BATCH_BEGIN_MARKER)
            .ok_or("unexpected output outside host_resource_v1 markers")?;
        let begin_line_end = begin
            .find('\n')
            .ok_or("unterminated host_resource_v1 begin marker")?;
        let action_id = &begin[..begin_line_end];
        if action_id.is_empty() || !seen.insert(action_id.to_owned()) {
            return Err("invalid or duplicate host_resource_v1 begin marker");
        }
        let output_start = cursor + BATCH_BEGIN_MARKER.len() + begin_line_end + 1;
        let end_relative = stdout[output_start..]
            .find(BATCH_END_MARKER)
            .ok_or("unterminated host_resource_v1 action output")?;
        let output_end = output_start + end_relative;
        let output = stdout[output_start..output_end]
            .trim_matches(['\r', '\n'])
            .to_owned();
        let end_start = output_end + BATCH_END_MARKER.len();
        let end_tail = &stdout[end_start..];
        let end_line_end = end_tail.find('\n').unwrap_or(end_tail.len());
        let (end_action, exit_code) = end_tail[..end_line_end]
            .rsplit_once(':')
            .ok_or("invalid host_resource_v1 end marker")?;
        let exit_code = exit_code
            .parse::<i32>()
            .map_err(|_| "invalid host_resource_v1 action exit code")?;
        if action_id != end_action {
            return Err("host_resource_v1 action markers do not match");
        }
        let action = host_resource_v1_profile()
            .into_iter()
            .filter_map(|step| match step {
                ProfileStep::Action(action) => Some(action),
                ProfileStep::LocalDelayMillis(_) => None,
            })
            .find(|action| action.action_id == action_id)
            .ok_or("unknown host_resource_v1 action")?;
        let source = if exit_code != 0 {
            RawSource::Unavailable(action_exit_quality(exit_code))
        } else if output.len() > action.max_stdout_bytes {
            RawSource::Unavailable(MetricQuality::ParseFailed)
        } else {
            RawSource::Output(output)
        };
        capture.set_action(action_id, source)?;
        cursor = end_start + end_line_end + usize::from(end_line_end < end_tail.len());
    }

    let expected = host_resource_v1_profile()
        .into_iter()
        .filter(|step| matches!(step, ProfileStep::Action(_)))
        .count();
    if seen.len() != expected {
        return Err("host_resource_v1 batch omitted a fixed action");
    }
    Ok(capture)
}

fn action_exit_quality(exit_code: i32) -> MetricQuality {
    match exit_code {
        124 => MetricQuality::TimedOut,
        126 => MetricQuality::PermissionDenied,
        127 => MetricQuality::Unsupported,
        _ => MetricQuality::Unsupported,
    }
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

const fn action(
    action_id: &'static str,
    family: MetricFamily,
    phase: CollectionPhase,
    command: &'static str,
    max_stdout_bytes: usize,
    max_items: usize,
    required_for_run: bool,
) -> ProfileStep {
    ProfileStep::Action(FixedAction {
        action_id,
        family,
        phase,
        command,
        max_stdout_bytes,
        max_items,
        required_for_run,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricQuality {
    Observed,
    Unsupported,
    ParseFailed,
    CounterReset,
    CounterUnreliable,
    InsufficientInterval,
    PermissionDenied,
    TimedOut,
}

impl MetricQuality {
    pub fn is_observed(self) -> bool {
        self == Self::Observed
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawSource {
    Output(String),
    Unavailable(MetricQuality),
}

impl RawSource {
    pub fn output(value: impl Into<String>) -> Self {
        Self::Output(value.into())
    }
}

impl Default for RawSource {
    fn default() -> Self {
        Self::Unavailable(MetricQuality::Unsupported)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CounterFrameCapture {
    pub boot_uptime: RawSource,
    pub cpu: RawSource,
    pub disk_io: RawSource,
    pub network: RawSource,
    pub network_identity: RawSource,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostResourceCapture {
    pub frame_a: CounterFrameCapture,
    pub frame_b: CounterFrameCapture,
    pub cpu_online: RawSource,
    pub memory: RawSource,
    pub load: RawSource,
    pub disk_capacity: RawSource,
    pub disk_inodes: RawSource,
    pub process_summary: RawSource,
}

impl HostResourceCapture {
    /// Maps an integration result back to the fixed action id. Unknown ids are
    /// rejected; they are never interpreted as commands or paths.
    pub fn set_action(&mut self, action_id: &str, source: RawSource) -> Result<(), &'static str> {
        let target = match action_id {
            "frame_a_boot_uptime" => &mut self.frame_a.boot_uptime,
            "frame_a_cpu" => &mut self.frame_a.cpu,
            "frame_a_disk_io" => &mut self.frame_a.disk_io,
            "frame_a_network" => &mut self.frame_a.network,
            "frame_a_network_identity" => &mut self.frame_a.network_identity,
            "frame_b_boot_uptime" => &mut self.frame_b.boot_uptime,
            "frame_b_cpu" => &mut self.frame_b.cpu,
            "frame_b_disk_io" => &mut self.frame_b.disk_io,
            "frame_b_network" => &mut self.frame_b.network,
            "frame_b_network_identity" => &mut self.frame_b.network_identity,
            "cpu_online" => &mut self.cpu_online,
            "memory" => &mut self.memory,
            "load" => &mut self.load,
            "disk_capacity" => &mut self.disk_capacity,
            "disk_inodes" => &mut self.disk_inodes,
            "process_summary" => &mut self.process_summary,
            _ => return Err("unknown host_resource_v1 action id"),
        };
        *target = source;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FamilyObservation<T> {
    pub quality: MetricQuality,
    pub value: Option<T>,
}

impl<T> FamilyObservation<T> {
    fn observed(value: T) -> Self {
        Self {
            quality: MetricQuality::Observed,
            value: Some(value),
        }
    }

    fn unavailable(quality: MetricQuality) -> Self {
        Self {
            quality,
            value: None,
        }
    }

    fn with_value(quality: MetricQuality, value: T) -> Self {
        Self {
            quality,
            value: Some(value),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunCompleteness {
    Succeeded,
    Partial,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionCoverage {
    pub required_observed: usize,
    pub required_total: usize,
    pub optional_observed: usize,
    pub optional_total: usize,
    pub completeness: RunCompleteness,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HostResourceObservation {
    pub profile: &'static str,
    pub protocol_version: u32,
    pub cpu: FamilyObservation<CpuMetrics>,
    /// Cumulative CPU ticks from counter frame B. Kept separately from the
    /// one-second derived rate so a reset or short interval does not discard
    /// the current counter baseline needed by later scheduled runs.
    pub cpu_counters: FamilyObservation<CpuCounterSnapshot>,
    pub memory: FamilyObservation<MemoryMetrics>,
    pub load: FamilyObservation<LoadMetrics>,
    pub disk_capacity: FamilyObservation<Vec<FilesystemCapacity>>,
    pub disk_io: FamilyObservation<Vec<BlockIoRate>>,
    /// Cumulative block-device counters from counter frame B.
    pub disk_io_counters: FamilyObservation<Vec<BlockIoCounterSnapshot>>,
    pub network: FamilyObservation<Vec<InterfaceRate>>,
    /// Cumulative interface counters from counter frame B.
    pub network_counters: FamilyObservation<Vec<InterfaceCounterSnapshot>>,
    pub uptime: FamilyObservation<UptimeMetrics>,
    pub process: FamilyObservation<ProcessSummary>,
    pub coverage: CollectionCoverage,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CpuTicks {
    pub user: u64,
    pub nice: u64,
    pub system: u64,
    pub idle: u64,
    pub iowait: u64,
    pub irq: u64,
    pub softirq: u64,
    pub steal: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuCoreCounterSnapshot {
    pub cpu: String,
    pub ticks: CpuTicks,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuCounterSnapshot {
    pub aggregate: CpuTicks,
    pub per_cpu: Vec<CpuCoreCounterSnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CpuRate {
    pub busy_pct: f64,
    pub iowait_pct: f64,
    pub steal_pct: f64,
    pub total_delta_ticks: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CpuCoreRate {
    pub cpu: String,
    pub quality: MetricQuality,
    pub rate: Option<CpuRate>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CpuMetrics {
    pub aggregate: CpuRate,
    pub per_cpu: Vec<CpuCoreRate>,
    pub online_cpu_count: Option<u32>,
    pub online_cpu_quality: MetricQuality,
    pub window_seconds: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryMetrics {
    pub total_kib: u64,
    pub available_kib: Option<u64>,
    pub used_kib: Option<u64>,
    pub swap_total_kib: Option<u64>,
    pub swap_free_kib: Option<u64>,
    pub cached_kib: Option<u64>,
    pub buffers_kib: Option<u64>,
    pub slab_kib: Option<u64>,
    pub derived_quality: MetricQuality,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoadMetrics {
    pub load1: f64,
    pub load5: f64,
    pub load15: f64,
    pub runnable_entities: u64,
    pub total_scheduling_entities: u64,
    pub online_cpu_count: Option<u32>,
    pub normalized_load1: Option<f64>,
    pub normalized_load5: Option<f64>,
    pub normalized_load15: Option<f64>,
    pub normalized_quality: MetricQuality,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FilesystemCapacity {
    pub source: String,
    pub mount: String,
    pub size_kib: u64,
    pub used_kib: u64,
    pub available_kib: u64,
    pub allocatable_used_ratio: Option<f64>,
    pub inode_total: Option<u64>,
    pub inode_used: Option<u64>,
    pub inode_available: Option<u64>,
    pub inode_used_ratio: Option<f64>,
    pub inode_quality: MetricQuality,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BlockIoMetrics {
    pub read_bytes_per_second: f64,
    pub write_bytes_per_second: f64,
    pub iops: f64,
    pub read_await_ms: Option<f64>,
    pub write_await_ms: Option<f64>,
    pub util_pct: f64,
    pub average_queue_depth: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BlockIoRate {
    pub identity: String,
    pub name: String,
    pub major: u32,
    pub minor: u32,
    pub quality: MetricQuality,
    pub window_seconds: f64,
    pub metrics: Option<BlockIoMetrics>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockIoCounterSnapshot {
    pub identity: String,
    pub name: String,
    pub major: u32,
    pub minor: u32,
    pub reads_completed: u64,
    pub read_sectors: u64,
    pub read_time_ms: u64,
    pub writes_completed: u64,
    pub write_sectors: u64,
    pub write_time_ms: u64,
    pub io_time_ms: u64,
    pub weighted_io_time_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InterfaceMetrics {
    pub rx_bytes_per_second: f64,
    pub tx_bytes_per_second: f64,
    pub rx_packets_per_second: f64,
    pub tx_packets_per_second: f64,
    pub rx_error_drop_pct: Option<f64>,
    pub tx_error_drop_pct: Option<f64>,
    pub rx_util_pct: Option<f64>,
    pub tx_util_pct: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InterfaceRate {
    pub identity: String,
    pub name: String,
    pub ifindex: u32,
    pub iflink: Option<u32>,
    pub operstate: String,
    pub speed_mbps: Option<u64>,
    pub quality: MetricQuality,
    pub window_seconds: f64,
    pub metrics: Option<InterfaceMetrics>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceCounterSnapshot {
    pub identity: String,
    pub name: String,
    pub ifindex: u32,
    pub rx_bytes: u64,
    pub rx_packets: u64,
    pub rx_errors: u64,
    pub rx_drops: u64,
    pub tx_bytes: u64,
    pub tx_packets: u64,
    pub tx_errors: u64,
    pub tx_drops: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UptimeMetrics {
    pub boot_id: String,
    pub uptime_seconds: f64,
    pub rebooted_since_frame_a: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSummary {
    pub scanned: u32,
    pub running: u32,
    pub blocked: u32,
    pub zombie: u32,
    pub raced: u32,
    pub truncated: bool,
}

#[derive(Debug, Clone)]
struct BootSample {
    boot_id: String,
    uptime_seconds: f64,
}

#[derive(Debug, Clone)]
struct CpuFrame {
    aggregate: CpuTicks,
    per_cpu: BTreeMap<String, CpuTicks>,
}

#[derive(Debug, Clone)]
struct DfRow {
    source: String,
    mount: String,
    total: u64,
    used: u64,
    available: u64,
}

#[derive(Debug, Clone)]
struct DiskCounters {
    major: u32,
    minor: u32,
    name: String,
    reads: u64,
    read_sectors: u64,
    read_ms: u64,
    writes: u64,
    write_sectors: u64,
    write_ms: u64,
    io_ms: u64,
    weighted_io_ms: u64,
}

#[derive(Debug, Clone)]
struct NetworkCounters {
    rx_bytes: u64,
    rx_packets: u64,
    rx_errors: u64,
    rx_drops: u64,
    tx_bytes: u64,
    tx_packets: u64,
    tx_errors: u64,
    tx_drops: u64,
}

#[derive(Debug, Clone)]
struct InterfaceIdentity {
    ifindex: u32,
    iflink: Option<u32>,
    operstate: String,
    speed_mbps: Option<u64>,
}

/// Parses every family independently. A missing or malformed optional family
/// never discards successful required families.
pub fn parse_host_resource_v1(capture: &HostResourceCapture) -> HostResourceObservation {
    let boot_a = parse_source(&capture.frame_a.boot_uptime, parse_boot_sample);
    let boot_b = parse_source(&capture.frame_b.boot_uptime, parse_boot_sample);
    let online_cpu = parse_source(&capture.cpu_online, parse_online_cpu_count);

    let uptime = derive_uptime(&boot_a, &boot_b);
    let cpu = derive_cpu(capture, &boot_a, &boot_b, &online_cpu);
    let cpu_counters = capture_cpu_counters(capture, &boot_b);
    let memory = derive_memory(&capture.memory);
    let load = derive_load(&capture.load, &online_cpu);
    let disk_capacity = derive_disk_capacity(&capture.disk_capacity, &capture.disk_inodes);
    let disk_io = derive_disk_io(capture, &boot_a, &boot_b);
    let disk_io_counters = capture_disk_io_counters(capture, &boot_b);
    let network = derive_network(capture, &boot_a, &boot_b);
    let network_counters = capture_network_counters(capture, &boot_b);
    let process = parse_source(&capture.process_summary, parse_process_summary);

    let required = [
        cpu.quality,
        memory.quality,
        load.quality,
        disk_capacity.quality,
        uptime.quality,
    ];
    let optional = [disk_io.quality, network.quality, process.quality];
    let required_observed = required
        .iter()
        .filter(|quality| quality.is_observed())
        .count();
    let optional_observed = optional
        .iter()
        .filter(|quality| quality.is_observed())
        .count();
    let completeness = if required_observed == required.len() {
        RunCompleteness::Succeeded
    } else if required_observed > 0 {
        RunCompleteness::Partial
    } else {
        RunCompleteness::Failed
    };

    HostResourceObservation {
        profile: HOST_RESOURCE_PROFILE_ID,
        protocol_version: HOST_RESOURCE_PROTOCOL_VERSION,
        cpu,
        cpu_counters,
        memory,
        load,
        disk_capacity,
        disk_io,
        disk_io_counters,
        network,
        network_counters,
        uptime,
        process,
        coverage: CollectionCoverage {
            required_observed,
            required_total: required.len(),
            optional_observed,
            optional_total: optional.len(),
            completeness,
        },
    }
}

fn parse_source<T>(source: &RawSource, parser: fn(&str) -> Result<T, ()>) -> FamilyObservation<T> {
    match source {
        RawSource::Output(output) => match parser(output) {
            Ok(value) => FamilyObservation::observed(value),
            Err(()) => FamilyObservation::unavailable(MetricQuality::ParseFailed),
        },
        RawSource::Unavailable(MetricQuality::Observed) => {
            FamilyObservation::unavailable(MetricQuality::ParseFailed)
        }
        RawSource::Unavailable(quality) => FamilyObservation::unavailable(*quality),
    }
}

fn capture_cpu_counters(
    capture: &HostResourceCapture,
    boot_b: &FamilyObservation<BootSample>,
) -> FamilyObservation<CpuCounterSnapshot> {
    let current = parse_source(&capture.frame_b.cpu, parse_proc_stat);
    let Some(frame) = current.value else {
        return FamilyObservation::unavailable(current.quality);
    };
    let snapshot = CpuCounterSnapshot {
        aggregate: frame.aggregate,
        per_cpu: frame
            .per_cpu
            .into_iter()
            .map(|(cpu, ticks)| CpuCoreCounterSnapshot { cpu, ticks })
            .collect(),
    };
    if boot_b.value.is_none() {
        // CPU subject ids deliberately stay stable across boots; the run's
        // boot_id is therefore the reset boundary used by H3b. Keep the series
        // keys and failure quality, but never persist an observed cumulative
        // value when that identity is unavailable.
        FamilyObservation::with_value(boot_b.quality, snapshot)
    } else {
        FamilyObservation::observed(snapshot)
    }
}

fn capture_disk_io_counters(
    capture: &HostResourceCapture,
    boot_b: &FamilyObservation<BootSample>,
) -> FamilyObservation<Vec<BlockIoCounterSnapshot>> {
    let current = parse_source(&capture.frame_b.disk_io, |output| {
        parse_diskstats(output, MAX_BLOCK_DEVICES)
    });
    let Some(devices) = current.value else {
        return FamilyObservation::unavailable(current.quality);
    };
    let Some(boot) = boot_b.value.as_ref() else {
        return FamilyObservation::unavailable(boot_b.quality);
    };
    FamilyObservation::observed(
        devices
            .into_iter()
            .map(|device| BlockIoCounterSnapshot {
                identity: format!("{}:{}:{}", boot.boot_id, device.major, device.minor),
                name: device.name,
                major: device.major,
                minor: device.minor,
                reads_completed: device.reads,
                read_sectors: device.read_sectors,
                read_time_ms: device.read_ms,
                writes_completed: device.writes,
                write_sectors: device.write_sectors,
                write_time_ms: device.write_ms,
                io_time_ms: device.io_ms,
                weighted_io_time_ms: device.weighted_io_ms,
            })
            .collect(),
    )
}

fn capture_network_counters(
    capture: &HostResourceCapture,
    boot_b: &FamilyObservation<BootSample>,
) -> FamilyObservation<Vec<InterfaceCounterSnapshot>> {
    let current_counters = parse_source(&capture.frame_b.network, |output| {
        parse_network(output, MAX_INTERFACES)
    });
    let current_identity = parse_source(&capture.frame_b.network_identity, |output| {
        parse_network_identity(output, MAX_INTERFACES)
    });
    let Some(counters_by_name) = current_counters.value else {
        return FamilyObservation::unavailable(current_counters.quality);
    };
    let Some(identity_by_name) = current_identity.value else {
        return FamilyObservation::unavailable(current_identity.quality);
    };
    let Some(boot) = boot_b.value.as_ref() else {
        return FamilyObservation::unavailable(boot_b.quality);
    };
    let mut counters = Vec::with_capacity(counters_by_name.len());
    for (name, values) in counters_by_name {
        let Some(identity) = identity_by_name.get(&name) else {
            // Interface inventory can race with /proc/net/dev. A member
            // without an ifindex cannot form a stable counter series, but it
            // must not erase other interfaces whose identities are complete.
            continue;
        };
        counters.push(InterfaceCounterSnapshot {
            identity: format!("{}:{}", boot.boot_id, identity.ifindex),
            name,
            ifindex: identity.ifindex,
            rx_bytes: values.rx_bytes,
            rx_packets: values.rx_packets,
            rx_errors: values.rx_errors,
            rx_drops: values.rx_drops,
            tx_bytes: values.tx_bytes,
            tx_packets: values.tx_packets,
            tx_errors: values.tx_errors,
            tx_drops: values.tx_drops,
        });
    }
    if counters.is_empty() {
        FamilyObservation::unavailable(MetricQuality::ParseFailed)
    } else {
        FamilyObservation::observed(counters)
    }
}

fn derive_uptime(
    boot_a: &FamilyObservation<BootSample>,
    boot_b: &FamilyObservation<BootSample>,
) -> FamilyObservation<UptimeMetrics> {
    let Some(current) = boot_b.value.as_ref() else {
        return FamilyObservation::unavailable(boot_b.quality);
    };
    FamilyObservation::observed(UptimeMetrics {
        boot_id: current.boot_id.clone(),
        uptime_seconds: current.uptime_seconds,
        rebooted_since_frame_a: boot_a
            .value
            .as_ref()
            .map(|previous| previous.boot_id != current.boot_id),
    })
}

fn derive_cpu(
    capture: &HostResourceCapture,
    boot_a: &FamilyObservation<BootSample>,
    boot_b: &FamilyObservation<BootSample>,
    online_cpu: &FamilyObservation<u32>,
) -> FamilyObservation<CpuMetrics> {
    let previous = parse_source(&capture.frame_a.cpu, parse_proc_stat);
    let current = parse_source(&capture.frame_b.cpu, parse_proc_stat);
    let (Some(previous_boot), Some(current_boot)) = (boot_a.value.as_ref(), boot_b.value.as_ref())
    else {
        return FamilyObservation::unavailable(first_unavailable(&[
            boot_a.quality,
            boot_b.quality,
        ]));
    };
    let Some(previous_frame) = previous.value.as_ref() else {
        return FamilyObservation::unavailable(previous.quality);
    };
    let Some(current_frame) = current.value.as_ref() else {
        return FamilyObservation::unavailable(current.quality);
    };
    if previous_boot.boot_id != current_boot.boot_id {
        return FamilyObservation::unavailable(MetricQuality::CounterReset);
    }
    let dt = current_boot.uptime_seconds - previous_boot.uptime_seconds;
    let aggregate = diff_cpu_ticks(&previous_frame.aggregate, &current_frame.aggregate, dt);
    let Some(aggregate_rate) = aggregate.value else {
        return FamilyObservation::unavailable(aggregate.quality);
    };
    let per_cpu = current_frame
        .per_cpu
        .iter()
        .map(|(name, current_ticks)| {
            let rate = previous_frame
                .per_cpu
                .get(name)
                .map(|previous_ticks| diff_cpu_ticks(previous_ticks, current_ticks, dt))
                .unwrap_or_else(|| FamilyObservation::unavailable(MetricQuality::CounterReset));
            CpuCoreRate {
                cpu: name.clone(),
                quality: rate.quality,
                rate: rate.value,
            }
        })
        .collect();

    FamilyObservation::observed(CpuMetrics {
        aggregate: aggregate_rate,
        per_cpu,
        online_cpu_count: online_cpu.value,
        online_cpu_quality: online_cpu.quality,
        window_seconds: dt,
    })
}

fn diff_cpu_ticks(previous: &CpuTicks, current: &CpuTicks, dt: f64) -> FamilyObservation<CpuRate> {
    if !valid_interval(dt) {
        return FamilyObservation::unavailable(MetricQuality::InsufficientInterval);
    }
    if current.user < previous.user
        || current.nice < previous.nice
        || current.system < previous.system
        || current.idle < previous.idle
        || current.irq < previous.irq
        || current.softirq < previous.softirq
        || current.steal < previous.steal
    {
        return FamilyObservation::unavailable(MetricQuality::CounterReset);
    }
    if current.iowait < previous.iowait {
        return FamilyObservation::unavailable(MetricQuality::CounterUnreliable);
    }
    let deltas = CpuTicks {
        user: current.user - previous.user,
        nice: current.nice - previous.nice,
        system: current.system - previous.system,
        idle: current.idle - previous.idle,
        iowait: current.iowait - previous.iowait,
        irq: current.irq - previous.irq,
        softirq: current.softirq - previous.softirq,
        steal: current.steal - previous.steal,
    };
    let Some(total) = cpu_total(&deltas) else {
        return FamilyObservation::unavailable(MetricQuality::CounterUnreliable);
    };
    if total == 0 {
        return FamilyObservation::unavailable(MetricQuality::InsufficientInterval);
    }
    let busy = total
        .saturating_sub(deltas.idle)
        .saturating_sub(deltas.iowait);
    FamilyObservation::observed(CpuRate {
        busy_pct: percent(busy, total),
        iowait_pct: percent(deltas.iowait, total),
        steal_pct: percent(deltas.steal, total),
        total_delta_ticks: total,
    })
}

fn derive_memory(source: &RawSource) -> FamilyObservation<MemoryMetrics> {
    let parsed = parse_source(source, parse_memory);
    let Some(memory) = parsed.value else {
        return FamilyObservation::unavailable(parsed.quality);
    };
    FamilyObservation::with_value(memory.derived_quality, memory)
}

fn derive_load(
    source: &RawSource,
    online_cpu: &FamilyObservation<u32>,
) -> FamilyObservation<LoadMetrics> {
    let parsed = parse_source(source, parse_load);
    let Some(mut load) = parsed.value else {
        return FamilyObservation::unavailable(parsed.quality);
    };
    if let Some(count) = online_cpu.value.filter(|count| *count > 0) {
        let divisor = f64::from(count);
        load.online_cpu_count = Some(count);
        load.normalized_load1 = Some(load.load1 / divisor);
        load.normalized_load5 = Some(load.load5 / divisor);
        load.normalized_load15 = Some(load.load15 / divisor);
        load.normalized_quality = MetricQuality::Observed;
    } else {
        load.normalized_quality = online_cpu.quality;
    }
    FamilyObservation::observed(load)
}

fn derive_disk_capacity(
    capacity_source: &RawSource,
    inode_source: &RawSource,
) -> FamilyObservation<Vec<FilesystemCapacity>> {
    let capacities = parse_source(capacity_source, |output| parse_df(output, MAX_FILESYSTEMS));
    let Some(capacities) = capacities.value else {
        return FamilyObservation::unavailable(capacities.quality);
    };
    let inodes = parse_source(inode_source, |output| parse_df(output, MAX_FILESYSTEMS));
    let inode_by_mount = inodes
        .value
        .unwrap_or_default()
        .into_iter()
        .map(|row| (row.mount.clone(), row))
        .collect::<BTreeMap<_, _>>();
    let filesystems = capacities
        .into_iter()
        .map(|row| {
            let inode = inode_by_mount.get(&row.mount);
            FilesystemCapacity {
                source: row.source,
                mount: row.mount,
                size_kib: row.total,
                used_kib: row.used,
                available_kib: row.available,
                allocatable_used_ratio: ratio(row.used, row.used.saturating_add(row.available)),
                inode_total: inode.map(|value| value.total),
                inode_used: inode.map(|value| value.used),
                inode_available: inode.map(|value| value.available),
                inode_used_ratio: inode.and_then(|value| {
                    ratio(value.used, value.used.saturating_add(value.available))
                }),
                inode_quality: if inode.is_some() {
                    MetricQuality::Observed
                } else {
                    inodes.quality
                },
            }
        })
        .collect();
    FamilyObservation::observed(filesystems)
}

fn derive_disk_io(
    capture: &HostResourceCapture,
    boot_a: &FamilyObservation<BootSample>,
    boot_b: &FamilyObservation<BootSample>,
) -> FamilyObservation<Vec<BlockIoRate>> {
    let previous = parse_source(&capture.frame_a.disk_io, |output| {
        parse_diskstats(output, MAX_BLOCK_DEVICES)
    });
    let current = parse_source(&capture.frame_b.disk_io, |output| {
        parse_diskstats(output, MAX_BLOCK_DEVICES)
    });
    let (Some(previous_boot), Some(current_boot)) = (boot_a.value.as_ref(), boot_b.value.as_ref())
    else {
        return FamilyObservation::unavailable(first_unavailable(&[
            boot_a.quality,
            boot_b.quality,
        ]));
    };
    let Some(previous_devices) = previous.value else {
        return FamilyObservation::unavailable(previous.quality);
    };
    let Some(current_devices) = current.value else {
        return FamilyObservation::unavailable(current.quality);
    };
    let dt = current_boot.uptime_seconds - previous_boot.uptime_seconds;
    let same_boot = previous_boot.boot_id == current_boot.boot_id;
    let previous_by_id = previous_devices
        .into_iter()
        .map(|device| ((device.major, device.minor), device))
        .collect::<BTreeMap<_, _>>();
    let mut rates = Vec::with_capacity(current_devices.len());
    for device in current_devices {
        let identity = format!("{}:{}:{}", current_boot.boot_id, device.major, device.minor);
        let observation = if !same_boot {
            FamilyObservation::unavailable(MetricQuality::CounterReset)
        } else if !valid_interval(dt) {
            FamilyObservation::unavailable(MetricQuality::InsufficientInterval)
        } else if let Some(previous) = previous_by_id.get(&(device.major, device.minor)) {
            diff_disk(previous, &device, dt)
        } else {
            FamilyObservation::unavailable(MetricQuality::CounterReset)
        };
        rates.push(BlockIoRate {
            identity,
            name: device.name,
            major: device.major,
            minor: device.minor,
            quality: observation.quality,
            window_seconds: dt.max(0.0),
            metrics: observation.value,
        });
    }
    family_from_members(rates, |rate| rate.quality)
}

fn diff_disk(
    previous: &DiskCounters,
    current: &DiskCounters,
    dt: f64,
) -> FamilyObservation<BlockIoMetrics> {
    if current.reads < previous.reads
        || current.read_sectors < previous.read_sectors
        || current.read_ms < previous.read_ms
        || current.writes < previous.writes
        || current.write_sectors < previous.write_sectors
        || current.write_ms < previous.write_ms
        || current.io_ms < previous.io_ms
        || current.weighted_io_ms < previous.weighted_io_ms
    {
        return FamilyObservation::unavailable(MetricQuality::CounterReset);
    }
    let reads = current.reads - previous.reads;
    let writes = current.writes - previous.writes;
    let read_ms = current.read_ms - previous.read_ms;
    let write_ms = current.write_ms - previous.write_ms;
    FamilyObservation::observed(BlockIoMetrics {
        read_bytes_per_second: (current.read_sectors - previous.read_sectors) as f64 * 512.0 / dt,
        write_bytes_per_second: (current.write_sectors - previous.write_sectors) as f64 * 512.0
            / dt,
        iops: reads.saturating_add(writes) as f64 / dt,
        read_await_ms: (reads > 0).then_some(read_ms as f64 / reads as f64),
        write_await_ms: (writes > 0).then_some(write_ms as f64 / writes as f64),
        util_pct: (current.io_ms - previous.io_ms) as f64 * 100.0 / (dt * 1_000.0),
        average_queue_depth: (current.weighted_io_ms - previous.weighted_io_ms) as f64
            / (dt * 1_000.0),
    })
}

fn derive_network(
    capture: &HostResourceCapture,
    boot_a: &FamilyObservation<BootSample>,
    boot_b: &FamilyObservation<BootSample>,
) -> FamilyObservation<Vec<InterfaceRate>> {
    let previous_counters = parse_source(&capture.frame_a.network, |output| {
        parse_network(output, MAX_INTERFACES)
    });
    let current_counters = parse_source(&capture.frame_b.network, |output| {
        parse_network(output, MAX_INTERFACES)
    });
    let previous_identity = parse_source(&capture.frame_a.network_identity, |output| {
        parse_network_identity(output, MAX_INTERFACES)
    });
    let current_identity = parse_source(&capture.frame_b.network_identity, |output| {
        parse_network_identity(output, MAX_INTERFACES)
    });
    let (Some(previous_boot), Some(current_boot)) = (boot_a.value.as_ref(), boot_b.value.as_ref())
    else {
        return FamilyObservation::unavailable(first_unavailable(&[
            boot_a.quality,
            boot_b.quality,
        ]));
    };
    let Some(previous_counters) = previous_counters.value else {
        return FamilyObservation::unavailable(previous_counters.quality);
    };
    let Some(current_counters) = current_counters.value else {
        return FamilyObservation::unavailable(current_counters.quality);
    };
    let Some(previous_identity) = previous_identity.value else {
        return FamilyObservation::unavailable(previous_identity.quality);
    };
    let Some(current_identity) = current_identity.value else {
        return FamilyObservation::unavailable(current_identity.quality);
    };

    let dt = current_boot.uptime_seconds - previous_boot.uptime_seconds;
    let same_boot = previous_boot.boot_id == current_boot.boot_id;
    let previous_by_ifindex = previous_identity
        .iter()
        .filter_map(|(name, identity)| {
            previous_counters
                .get(name)
                .map(|counters| (identity.ifindex, counters))
        })
        .collect::<BTreeMap<_, _>>();
    let mut rates = Vec::with_capacity(current_counters.len());
    for (name, counters) in current_counters {
        let Some(identity) = current_identity.get(&name) else {
            rates.push(InterfaceRate {
                identity: format!("{}:unknown:{}", current_boot.boot_id, name),
                name,
                ifindex: 0,
                iflink: None,
                operstate: "unknown".to_owned(),
                speed_mbps: None,
                quality: MetricQuality::ParseFailed,
                window_seconds: dt.max(0.0),
                metrics: None,
            });
            continue;
        };
        let observation = if !same_boot {
            FamilyObservation::unavailable(MetricQuality::CounterReset)
        } else if !valid_interval(dt) {
            FamilyObservation::unavailable(MetricQuality::InsufficientInterval)
        } else if let Some(previous) = previous_by_ifindex.get(&identity.ifindex) {
            diff_network(previous, &counters, identity.speed_mbps, dt)
        } else {
            FamilyObservation::unavailable(MetricQuality::CounterReset)
        };
        rates.push(InterfaceRate {
            identity: format!("{}:{}", current_boot.boot_id, identity.ifindex),
            name,
            ifindex: identity.ifindex,
            iflink: identity.iflink,
            operstate: identity.operstate.clone(),
            speed_mbps: identity.speed_mbps,
            quality: observation.quality,
            window_seconds: dt.max(0.0),
            metrics: observation.value,
        });
    }
    family_from_members(rates, |rate| rate.quality)
}

fn diff_network(
    previous: &NetworkCounters,
    current: &NetworkCounters,
    speed_mbps: Option<u64>,
    dt: f64,
) -> FamilyObservation<InterfaceMetrics> {
    if current.rx_bytes < previous.rx_bytes
        || current.rx_packets < previous.rx_packets
        || current.rx_errors < previous.rx_errors
        || current.rx_drops < previous.rx_drops
        || current.tx_bytes < previous.tx_bytes
        || current.tx_packets < previous.tx_packets
        || current.tx_errors < previous.tx_errors
        || current.tx_drops < previous.tx_drops
    {
        return FamilyObservation::unavailable(MetricQuality::CounterReset);
    }
    let rx_bytes = current.rx_bytes - previous.rx_bytes;
    let tx_bytes = current.tx_bytes - previous.tx_bytes;
    let rx_packets = current.rx_packets - previous.rx_packets;
    let tx_packets = current.tx_packets - previous.tx_packets;
    let rx_bad = (current.rx_errors - previous.rx_errors)
        .saturating_add(current.rx_drops - previous.rx_drops);
    let tx_bad = (current.tx_errors - previous.tx_errors)
        .saturating_add(current.tx_drops - previous.tx_drops);
    let rx_bytes_per_second = rx_bytes as f64 / dt;
    let tx_bytes_per_second = tx_bytes as f64 / dt;
    let bits_per_second_capacity = speed_mbps
        .filter(|speed| *speed > 0)
        .map(|speed| speed as f64 * 1_000_000.0);
    FamilyObservation::observed(InterfaceMetrics {
        rx_bytes_per_second,
        tx_bytes_per_second,
        rx_packets_per_second: rx_packets as f64 / dt,
        tx_packets_per_second: tx_packets as f64 / dt,
        rx_error_drop_pct: (rx_packets > 0).then_some(percent(rx_bad, rx_packets)),
        tx_error_drop_pct: (tx_packets > 0).then_some(percent(tx_bad, tx_packets)),
        rx_util_pct: bits_per_second_capacity
            .map(|capacity| rx_bytes_per_second * 8.0 * 100.0 / capacity),
        tx_util_pct: bits_per_second_capacity
            .map(|capacity| tx_bytes_per_second * 8.0 * 100.0 / capacity),
    })
}

fn family_from_members<T>(
    members: Vec<T>,
    quality: impl Fn(&T) -> MetricQuality,
) -> FamilyObservation<Vec<T>> {
    if members.is_empty() {
        return FamilyObservation::unavailable(MetricQuality::Unsupported);
    }
    if members.iter().any(|member| quality(member).is_observed()) {
        return FamilyObservation::observed(members);
    }
    let quality = members
        .iter()
        .map(quality)
        .next()
        .unwrap_or(MetricQuality::Unsupported);
    FamilyObservation::with_value(quality, members)
}

fn parse_boot_sample(output: &str) -> Result<BootSample, ()> {
    let mut lines = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let boot_id = lines.next().ok_or(())?;
    if boot_id.len() > 128
        || !boot_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(());
    }
    let uptime_line = lines.next().ok_or(())?;
    let uptime_seconds = parse_f64(uptime_line.split_whitespace().next().ok_or(())?)?;
    if uptime_seconds < 0.0 {
        return Err(());
    }
    Ok(BootSample {
        boot_id: boot_id.to_owned(),
        uptime_seconds,
    })
}

fn parse_proc_stat(output: &str) -> Result<CpuFrame, ()> {
    let mut aggregate = None;
    let mut per_cpu = BTreeMap::new();
    for line in output.lines() {
        let mut fields = line.split_whitespace();
        let Some(name) = fields.next() else { continue };
        if name != "cpu"
            && !name.strip_prefix("cpu").is_some_and(|suffix| {
                !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
            })
        {
            continue;
        }
        let mut values = [0u64; 8];
        for value in &mut values {
            *value = fields.next().ok_or(())?.parse().map_err(|_| ())?;
        }
        let ticks = CpuTicks {
            user: values[0],
            nice: values[1],
            system: values[2],
            idle: values[3],
            iowait: values[4],
            irq: values[5],
            softirq: values[6],
            steal: values[7],
        };
        cpu_total(&ticks).ok_or(())?;
        if name == "cpu" {
            aggregate = Some(ticks);
        } else {
            per_cpu.insert(name.to_owned(), ticks);
        }
    }
    Ok(CpuFrame {
        aggregate: aggregate.ok_or(())?,
        per_cpu,
    })
}

fn parse_online_cpu_count(output: &str) -> Result<u32, ()> {
    let value = output.trim();
    let (kind, value) = value.split_once(' ').unwrap_or(("legacy", value));
    if kind == "count" {
        let count = value.parse::<u32>().map_err(|_| ())?;
        return (count > 0).then_some(count).ok_or(());
    }
    if kind != "list" && kind != "legacy" {
        return Err(());
    }
    if kind == "legacy"
        && let Ok(count) = value.parse::<u32>()
        && count > 0
    {
        return Ok(count);
    }
    let mut count = 0u32;
    for part in value.split(',') {
        let mut range = part.trim().split('-');
        let start: u32 = range.next().ok_or(())?.parse().map_err(|_| ())?;
        let end = range
            .next()
            .map(|value| value.parse::<u32>().map_err(|_| ()))
            .transpose()?
            .unwrap_or(start);
        if range.next().is_some() || end < start {
            return Err(());
        }
        count = count.checked_add(end - start + 1).ok_or(())?;
    }
    (count > 0).then_some(count).ok_or(())
}

fn parse_memory(output: &str) -> Result<MemoryMetrics, ()> {
    let mut values = BTreeMap::new();
    for line in output.lines() {
        let Some((key, raw)) = line.split_once(':') else {
            continue;
        };
        let mut fields = raw.split_whitespace();
        let value: u64 = fields.next().ok_or(())?.parse().map_err(|_| ())?;
        if let Some(unit) = fields.next()
            && unit != "kB"
        {
            return Err(());
        }
        values.insert(key.trim(), value);
    }
    let total = *values.get("MemTotal").ok_or(())?;
    let available = values.get("MemAvailable").copied();
    if available.is_some_and(|value| value > total) {
        return Err(());
    }
    let used = available.map(|value| total - value);
    let derived_quality = if available.is_some() {
        MetricQuality::Observed
    } else {
        MetricQuality::Unsupported
    };
    Ok(MemoryMetrics {
        total_kib: total,
        available_kib: available,
        used_kib: used,
        swap_total_kib: values.get("SwapTotal").copied(),
        swap_free_kib: values.get("SwapFree").copied(),
        cached_kib: values.get("Cached").copied(),
        buffers_kib: values.get("Buffers").copied(),
        slab_kib: values.get("Slab").copied(),
        derived_quality,
    })
}

fn parse_load(output: &str) -> Result<LoadMetrics, ()> {
    let mut fields = output.split_whitespace();
    let load1 = parse_f64(fields.next().ok_or(())?)?;
    let load5 = parse_f64(fields.next().ok_or(())?)?;
    let load15 = parse_f64(fields.next().ok_or(())?)?;
    let entities = fields.next().ok_or(())?;
    let (runnable, total) = entities.split_once('/').ok_or(())?;
    Ok(LoadMetrics {
        load1,
        load5,
        load15,
        runnable_entities: runnable.parse().map_err(|_| ())?,
        total_scheduling_entities: total.parse().map_err(|_| ())?,
        online_cpu_count: None,
        normalized_load1: None,
        normalized_load5: None,
        normalized_load15: None,
        normalized_quality: MetricQuality::Unsupported,
    })
}

fn parse_df(output: &str, max_items: usize) -> Result<Vec<DfRow>, ()> {
    let mut lines = output.lines().filter(|line| !line.trim().is_empty());
    let header = lines.next().ok_or(())?;
    if !header.to_ascii_lowercase().contains("filesystem") {
        return Err(());
    }
    let mut rows = Vec::new();
    for line in lines {
        if rows.len() >= max_items {
            return Err(());
        }
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 6 {
            return Err(());
        }
        rows.push(DfRow {
            source: fields[0].to_owned(),
            total: fields[1].parse().map_err(|_| ())?,
            used: fields[2].parse().map_err(|_| ())?,
            available: fields[3].parse().map_err(|_| ())?,
            mount: fields[5..].join(" "),
        });
    }
    (!rows.is_empty()).then_some(rows).ok_or(())
}

fn parse_diskstats(output: &str, max_items: usize) -> Result<Vec<DiskCounters>, ()> {
    let mut devices = Vec::new();
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        if devices.len() >= max_items {
            return Err(());
        }
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 14 {
            return Err(());
        }
        devices.push(DiskCounters {
            major: fields[0].parse().map_err(|_| ())?,
            minor: fields[1].parse().map_err(|_| ())?,
            name: fields[2].to_owned(),
            reads: fields[3].parse().map_err(|_| ())?,
            read_sectors: fields[5].parse().map_err(|_| ())?,
            read_ms: fields[6].parse().map_err(|_| ())?,
            writes: fields[7].parse().map_err(|_| ())?,
            write_sectors: fields[9].parse().map_err(|_| ())?,
            write_ms: fields[10].parse().map_err(|_| ())?,
            io_ms: fields[12].parse().map_err(|_| ())?,
            weighted_io_ms: fields[13].parse().map_err(|_| ())?,
        });
    }
    (!devices.is_empty()).then_some(devices).ok_or(())
}

fn parse_network(output: &str, max_items: usize) -> Result<BTreeMap<String, NetworkCounters>, ()> {
    let mut interfaces = BTreeMap::new();
    for line in output.lines() {
        let Some((name, payload)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            return Err(());
        }
        if interfaces.len() >= max_items {
            return Err(());
        }
        let fields = payload.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 16 {
            return Err(());
        }
        interfaces.insert(
            name.to_owned(),
            NetworkCounters {
                rx_bytes: fields[0].parse().map_err(|_| ())?,
                rx_packets: fields[1].parse().map_err(|_| ())?,
                rx_errors: fields[2].parse().map_err(|_| ())?,
                rx_drops: fields[3].parse().map_err(|_| ())?,
                tx_bytes: fields[8].parse().map_err(|_| ())?,
                tx_packets: fields[9].parse().map_err(|_| ())?,
                tx_errors: fields[10].parse().map_err(|_| ())?,
                tx_drops: fields[11].parse().map_err(|_| ())?,
            },
        );
    }
    (!interfaces.is_empty()).then_some(interfaces).ok_or(())
}

fn parse_network_identity(
    output: &str,
    max_items: usize,
) -> Result<BTreeMap<String, InterfaceIdentity>, ()> {
    let mut interfaces = BTreeMap::new();
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        if interfaces.len() >= max_items {
            return Err(());
        }
        // This fixed collector emits tab-separated fields. Keep an empty final
        // field because common sysfs interfaces such as `lo` do not expose a
        // speed and some POSIX shells clear the read target on EINVAL.
        let fields = line.trim_end_matches('\r').split('\t').collect::<Vec<_>>();
        if fields.len() != 5 || fields[0].is_empty() {
            return Err(());
        }
        let ifindex: u32 = fields[1].parse().map_err(|_| ())?;
        if ifindex == 0 {
            return Err(());
        }
        let iflink = parse_optional_positive(fields[2]);
        let speed_mbps = fields[4]
            .parse::<i64>()
            .ok()
            .filter(|speed| *speed > 0)
            .map(|speed| speed as u64);
        interfaces.insert(
            fields[0].to_owned(),
            InterfaceIdentity {
                ifindex,
                iflink,
                operstate: fields[3].to_owned(),
                speed_mbps,
            },
        );
    }
    (!interfaces.is_empty()).then_some(interfaces).ok_or(())
}

fn parse_process_summary(output: &str) -> Result<ProcessSummary, ()> {
    let mut values = BTreeMap::new();
    for field in output.split_whitespace() {
        let (key, value) = field.split_once('=').ok_or(())?;
        values.insert(key, value);
    }
    let scanned = parse_u32(values.get("scanned").copied())?;
    if scanned as usize > MAX_PROCESS_ENTRIES {
        return Err(());
    }
    let running = parse_u32(values.get("running").copied())?;
    let blocked = parse_u32(values.get("blocked").copied())?;
    let zombie = parse_u32(values.get("zombie").copied())?;
    if running.saturating_add(blocked).saturating_add(zombie) > scanned {
        return Err(());
    }
    Ok(ProcessSummary {
        scanned,
        running,
        blocked,
        zombie,
        raced: parse_u32(values.get("raced").copied())?,
        truncated: match values.get("truncated").copied() {
            Some("0") => false,
            Some("1") => true,
            _ => return Err(()),
        },
    })
}

fn cpu_total(ticks: &CpuTicks) -> Option<u64> {
    [
        ticks.user,
        ticks.nice,
        ticks.system,
        ticks.idle,
        ticks.iowait,
        ticks.irq,
        ticks.softirq,
        ticks.steal,
    ]
    .into_iter()
    .try_fold(0u64, u64::checked_add)
}

fn first_unavailable(qualities: &[MetricQuality]) -> MetricQuality {
    qualities
        .iter()
        .copied()
        .find(|quality| !quality.is_observed())
        .unwrap_or(MetricQuality::ParseFailed)
}

fn valid_interval(dt: f64) -> bool {
    dt.is_finite() && dt >= MIN_COUNTER_INTERVAL_SECONDS
}

fn percent(numerator: u64, denominator: u64) -> f64 {
    numerator as f64 * 100.0 / denominator as f64
}

fn ratio(numerator: u64, denominator: u64) -> Option<f64> {
    (denominator > 0).then_some(numerator as f64 / denominator as f64)
}

fn parse_f64(value: &str) -> Result<f64, ()> {
    let value: f64 = value.parse().map_err(|_| ())?;
    value.is_finite().then_some(value).ok_or(())
}

fn parse_optional_positive(value: &str) -> Option<u32> {
    value.parse::<u32>().ok().filter(|value| *value > 0)
}

fn parse_u32(value: Option<&str>) -> Result<u32, ()> {
    value.ok_or(())?.parse().map_err(|_| ())
}
