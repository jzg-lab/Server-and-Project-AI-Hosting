#[path = "../src/monitoring.rs"]
mod monitoring;

use std::collections::BTreeSet;

use monitoring::{
    COUNTER_SAMPLE_DELAY_MILLIS, HostResourceCapture, MetricQuality, ProfileStep, RawSource,
    RunCompleteness, host_resource_v1_batch_command, host_resource_v1_profile,
    parse_host_resource_v1, parse_host_resource_v1_batch,
};

fn required_capture() -> HostResourceCapture {
    let mut capture = HostResourceCapture::default();
    capture.frame_a.boot_uptime = RawSource::output("boot-a\n100.0 0.0\n");
    capture.frame_b.boot_uptime = RawSource::output("boot-a\n101.0 0.0\n");
    capture.frame_a.cpu = RawSource::output(
        "cpu 100 10 50 800 20 10 5 5 900 800\n\
         cpu0 50 5 25 400 10 5 2 3 450 400\n\
         cpu1 50 5 25 400 10 5 3 2 450 400\n",
    );
    capture.frame_b.cpu = RawSource::output(
        "cpu 150 10 70 850 30 10 10 10 5000 4000\n\
         cpu0 75 5 35 425 15 5 5 5 2500 2000\n\
         cpu1 75 5 35 425 15 5 5 5 2500 2000\n",
    );
    capture.cpu_online = RawSource::output("0-1\n");
    capture.memory = RawSource::output(
        "MemTotal:       1000000 kB\n\
         MemAvailable:   400000 kB\n\
         SwapTotal:      200000 kB\n\
         SwapFree:       150000 kB\n\
         Cached:         120000 kB\n\
         Buffers:         10000 kB\n\
         Slab:            30000 kB\n",
    );
    capture.load = RawSource::output("0.50 1.00 1.50 2/100 1234\n");
    capture.disk_capacity = RawSource::output(
        "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
         /dev/sda1 1000 400 500 45% /\n",
    );
    capture.disk_inodes = RawSource::output(
        "Filesystem Inodes IUsed IFree IUse% Mounted on\n\
         /dev/sda1 1000 400 500 45% /\n",
    );
    capture
}

fn full_capture() -> HostResourceCapture {
    let mut capture = required_capture();
    capture.frame_a.disk_io = RawSource::output("8 0 sda 100 0 200 50 300 0 400 60 0 70 80\n");
    capture.frame_b.disk_io = RawSource::output("8 0 sda 110 0 220 55 305 0 430 70 0 90 110\n");
    capture.frame_a.network = RawSource::output(
        "Inter-| Receive | Transmit\n\
         face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs drop fifo colls carrier compressed\n\
         eth0: 1000 10 0 0 0 0 0 0 2000 20 0 0 0 0 0 0\n",
    );
    capture.frame_b.network = RawSource::output(
        "Inter-| Receive | Transmit\n\
         face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs drop fifo colls carrier compressed\n\
         eth0: 3000 30 1 0 0 0 0 0 5000 50 0 1 0 0 0 0\n",
    );
    capture.frame_a.network_identity = RawSource::output("eth0\t2\t2\tup\t1000\n");
    capture.frame_b.network_identity = RawSource::output("eth0\t2\t2\tup\t1000\n");
    capture.process_summary =
        RawSource::output("scanned=120 running=3 blocked=2 zombie=1 raced=4 truncated=0\n");
    capture
}

fn close(left: f64, right: f64) {
    assert!((left - right).abs() < 1e-9, "left={left} right={right}");
}

#[test]
fn fixed_profile_is_closed_bounded_and_read_only() {
    let profile = host_resource_v1_profile();
    let mut ids = BTreeSet::new();
    let mut delays = Vec::new();
    let mut commands = Vec::new();

    for step in profile {
        match step {
            ProfileStep::Action(action) => {
                assert!(
                    ids.insert(action.action_id),
                    "duplicate {}",
                    action.action_id
                );
                assert!(action.command.starts_with("LC_ALL=C "));
                assert!(action.max_stdout_bytes > 0);
                assert!(action.max_items > 0);
                commands.push(action.command.to_ascii_lowercase());
            }
            ProfileStep::LocalDelayMillis(delay) => delays.push(delay),
        }
    }

    assert_eq!(delays, [COUNTER_SAMPLE_DELAY_MILLIS]);
    assert_eq!(ids.len(), 16);
    for command in commands {
        for forbidden in [
            "sudo",
            "/cmdline",
            "/environ",
            "authorized_keys",
            "apt install",
            "yum install",
            "dnf install",
        ] {
            assert!(
                !command.contains(forbidden),
                "forbidden {forbidden}: {command}"
            );
        }
    }
}

#[test]
fn batch_command_is_one_closed_remote_profile_without_user_slots() {
    let command = host_resource_v1_batch_command();
    assert!(command.starts_with("LC_ALL=C sh -c '"));
    assert_eq!(command.matches("sleep 1.000").count(), 1);
    assert_eq!(
        command
            .matches("__NETWORK_ATLAS_HOST_RESOURCE_V1_BEGIN__")
            .count(),
        16
    );
    assert!(!command.contains("{host"));
    assert!(!command.contains("{command"));
    assert!(!command.contains("sudo"));
    assert!(command.contains("|| speed=-"));
    assert!(command.contains("printf \"list \""));
    assert!(command.contains("printf \"count \""));
    assert_eq!(command.matches("timeout 3 sh -c").count(), 2);
    assert_eq!(command.matches("timeout 2 sh -c").count(), 1);
    // Three bounded actions plus the pre-existing CPU-count fallback all
    // classify a missing utility as unsupported instead of running unbounded.
    assert_eq!(command.matches("else exit 127; fi").count(), 4);
}

#[test]
fn batch_output_preserves_partial_action_quality() {
    let mut output = String::new();
    for step in host_resource_v1_profile() {
        let ProfileStep::Action(action) = step else {
            continue;
        };
        output.push_str("__NETWORK_ATLAS_HOST_RESOURCE_V1_BEGIN__");
        output.push_str(action.action_id);
        output.push('\n');
        let (body, exit_code) = match action.action_id {
            "frame_a_boot_uptime" => ("boot-a\n100.0 0.0", 0),
            "frame_b_boot_uptime" => ("boot-a\n101.0 0.0", 0),
            "frame_a_cpu" => ("cpu 100 10 50 800 20 10 5 5 0 0", 0),
            "frame_b_cpu" => ("cpu 150 10 70 850 30 10 10 10 0 0", 0),
            "cpu_online" => ("0-1", 0),
            "memory" => ("MemTotal: 1000 kB\nMemAvailable: 500 kB", 0),
            "load" => ("0.5 1.0 1.5 1/10 7", 0),
            "disk_capacity" => (
                "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/x 100 20 70 23% /",
                0,
            ),
            "frame_a_network" => ("", 126),
            "frame_b_network" => ("", 126),
            "frame_a_network_identity" => ("", 126),
            "frame_b_network_identity" => ("", 126),
            _ => ("", 127),
        };
        if !body.is_empty() {
            output.push_str(body);
            output.push('\n');
        }
        output.push_str("__NETWORK_ATLAS_HOST_RESOURCE_V1_END__");
        output.push_str(action.action_id);
        output.push(':');
        output.push_str(&exit_code.to_string());
        output.push('\n');
    }

    let capture = parse_host_resource_v1_batch(&output).expect("closed batch output");
    assert_eq!(
        capture.frame_a.network,
        RawSource::Unavailable(MetricQuality::PermissionDenied)
    );
    let observation = parse_host_resource_v1(&capture);
    assert_eq!(
        observation.coverage.completeness,
        RunCompleteness::Succeeded
    );
    assert_eq!(observation.network.quality, MetricQuality::PermissionDenied);
}

#[test]
fn batch_output_limit_degrades_only_the_oversized_action() {
    let mut output = String::new();
    for step in host_resource_v1_profile() {
        let ProfileStep::Action(action) = step else {
            continue;
        };
        output.push_str("__NETWORK_ATLAS_HOST_RESOURCE_V1_BEGIN__");
        output.push_str(action.action_id);
        output.push('\n');
        match action.action_id {
            "frame_a_boot_uptime" => output.push_str("boot-a\n100.0 0.0\n"),
            "frame_b_boot_uptime" => output.push_str("boot-a\n101.0 0.0\n"),
            "frame_a_cpu" => output.push_str("cpu 100 10 50 800 20 10 5 5 0 0\n"),
            "frame_b_cpu" => output.push_str("cpu 150 10 70 850 30 10 10 10 0 0\n"),
            "cpu_online" => output.push_str("0-1\n"),
            "memory" => {
                output.extend(std::iter::repeat_n('x', action.max_stdout_bytes + 1));
                output.push('\n');
            }
            "load" => output.push_str("0.5 1.0 1.5 1/10 7\n"),
            "disk_capacity" => output
                .push_str("Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/x 100 20 70 23% /\n"),
            _ => {}
        }
        output.push_str("__NETWORK_ATLAS_HOST_RESOURCE_V1_END__");
        output.push_str(action.action_id);
        output.push_str(":0\n");
    }

    let capture = parse_host_resource_v1_batch(&output).expect("closed batch output");
    assert_eq!(
        capture.memory,
        RawSource::Unavailable(MetricQuality::ParseFailed)
    );
    assert!(matches!(capture.frame_a.cpu, RawSource::Output(_)));
}

#[test]
fn cpu_delta_matches_manual_math_and_ignores_guest_columns() {
    let observation = parse_host_resource_v1(&required_capture());
    assert_eq!(observation.cpu.quality, MetricQuality::Observed);
    let cpu = observation.cpu.value.expect("CPU metrics");

    assert_eq!(cpu.aggregate.total_delta_ticks, 140);
    close(cpu.aggregate.busy_pct, 100.0 * 80.0 / 140.0);
    close(cpu.aggregate.iowait_pct, 100.0 * 10.0 / 140.0);
    close(cpu.aggregate.steal_pct, 100.0 * 5.0 / 140.0);
    assert_eq!(cpu.online_cpu_count, Some(2));
    assert_eq!(cpu.per_cpu.len(), 2);
}

#[test]
fn same_boot_iowait_regression_is_unreliable_not_reboot_or_zero() {
    let mut capture = required_capture();
    capture.frame_b.cpu = RawSource::output("cpu 150 10 70 850 19 10 10 10 5000 4000\n");

    let observation = parse_host_resource_v1(&capture);
    assert_eq!(observation.cpu.quality, MetricQuality::CounterUnreliable);
    assert!(observation.cpu.value.is_none());
    assert_eq!(
        observation
            .uptime
            .value
            .expect("uptime")
            .rebooted_since_frame_a,
        Some(false)
    );
}

#[test]
fn boot_change_resets_counter_families_but_preserves_current_uptime() {
    let mut capture = full_capture();
    capture.frame_b.boot_uptime = RawSource::output("boot-b\n2.0 0.0\n");

    let observation = parse_host_resource_v1(&capture);
    assert_eq!(observation.cpu.quality, MetricQuality::CounterReset);
    assert_eq!(observation.disk_io.quality, MetricQuality::CounterReset);
    assert_eq!(observation.network.quality, MetricQuality::CounterReset);
    assert_eq!(observation.uptime.quality, MetricQuality::Observed);
    let uptime = observation.uptime.value.expect("current uptime");
    assert_eq!(uptime.boot_id, "boot-b");
    assert_eq!(uptime.rebooted_since_frame_a, Some(true));
}

#[test]
fn too_short_uptime_window_is_insufficient_instead_of_a_zero_rate() {
    let mut capture = required_capture();
    capture.frame_b.boot_uptime = RawSource::output("boot-a\n100.1 0.0\n");

    let observation = parse_host_resource_v1(&capture);
    assert_eq!(observation.cpu.quality, MetricQuality::InsufficientInterval);
    assert!(observation.cpu.value.is_none());
}

#[test]
fn missing_mem_available_is_unsupported_without_an_implicit_formula() {
    let mut capture = required_capture();
    capture.memory = RawSource::output(
        "MemTotal: 1000000 kB\nCached: 120000 kB\nBuffers: 10000 kB\nSlab: 30000 kB\n",
    );

    let observation = parse_host_resource_v1(&capture);
    assert_eq!(observation.memory.quality, MetricQuality::Unsupported);
    let memory = observation
        .memory
        .value
        .expect("raw memory remains visible");
    assert_eq!(memory.total_kib, 1_000_000);
    assert_eq!(memory.available_kib, None);
    assert_eq!(memory.used_kib, None);
    assert_eq!(observation.coverage.completeness, RunCompleteness::Partial);
}

#[test]
fn single_cpu_sysfs_list_zero_means_one_online_cpu() {
    let mut capture = required_capture();
    capture.cpu_online = RawSource::output("list 0\n");

    let observation = parse_host_resource_v1(&capture);
    let cpu = observation.cpu.value.expect("CPU metrics");
    assert_eq!(cpu.online_cpu_count, Some(1));
    let load = observation.load.value.expect("load metrics");
    assert_eq!(load.online_cpu_count, Some(1));
    close(load.normalized_load1.expect("normalized load"), 0.5);
}

#[test]
fn dash_loopback_empty_speed_keeps_network_identity_parseable() {
    let mut capture = required_capture();
    capture.frame_a.network = RawSource::output(
        "Inter-| Receive | Transmit\n\
         face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs drop fifo colls carrier compressed\n\
         lo: 1000 10 0 0 0 0 0 0 1000 10 0 0 0 0 0 0\n",
    );
    capture.frame_b.network = RawSource::output(
        "Inter-| Receive | Transmit\n\
         face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs drop fifo colls carrier compressed\n\
         lo: 3000 30 0 0 0 0 0 0 3000 30 0 0 0 0 0 0\n",
    );
    // Ubuntu's /bin/sh (dash) clears `speed` when reading lo/speed returns
    // EINVAL, so the real v1 batch emitted a trailing empty tab field.
    capture.frame_a.network_identity = RawSource::output("lo\t1\t1\tunknown\t\n");
    capture.frame_b.network_identity = RawSource::output("lo\t1\t1\tunknown\t\n");

    let observation = parse_host_resource_v1(&capture);
    assert_eq!(observation.network.quality, MetricQuality::Observed);
    let interfaces = observation.network.value.expect("network interfaces");
    assert_eq!(interfaces.len(), 1);
    assert_eq!(interfaces[0].name, "lo");
    assert_eq!(interfaces[0].speed_mbps, None);
    assert!(interfaces[0].metrics.is_some());
}

#[test]
fn changed_ifindex_creates_a_new_identity_without_cross_entity_rate() {
    let mut capture = full_capture();
    capture.frame_b.network_identity = RawSource::output("eth0\t3\t3\tup\t1000\n");

    let observation = parse_host_resource_v1(&capture);
    assert_eq!(observation.network.quality, MetricQuality::CounterReset);
    let interfaces = observation
        .network
        .value
        .expect("current interface retained");
    assert_eq!(interfaces.len(), 1);
    assert_eq!(interfaces[0].identity, "boot-a:3");
    assert_eq!(interfaces[0].quality, MetricQuality::CounterReset);
    assert!(interfaces[0].metrics.is_none());
}

#[test]
fn optional_families_can_be_unsupported_without_downgrading_a_complete_run() {
    let observation = parse_host_resource_v1(&required_capture());

    assert_eq!(observation.disk_io.quality, MetricQuality::Unsupported);
    assert_eq!(observation.network.quality, MetricQuality::Unsupported);
    assert_eq!(observation.process.quality, MetricQuality::Unsupported);
    assert_eq!(observation.coverage.required_observed, 5);
    assert_eq!(observation.coverage.optional_observed, 0);
    assert_eq!(
        observation.coverage.completeness,
        RunCompleteness::Succeeded
    );
}

#[test]
fn action_failures_remain_local_to_their_metric_families() {
    let mut capture = required_capture();
    capture.frame_a.disk_io = RawSource::Unavailable(MetricQuality::TimedOut);
    capture.frame_b.disk_io = RawSource::Unavailable(MetricQuality::TimedOut);
    capture.process_summary = RawSource::Unavailable(MetricQuality::PermissionDenied);

    let observation = parse_host_resource_v1(&capture);
    assert_eq!(observation.disk_io.quality, MetricQuality::TimedOut);
    assert_eq!(observation.process.quality, MetricQuality::PermissionDenied);
    assert_eq!(observation.cpu.quality, MetricQuality::Observed);
    assert_eq!(
        observation.coverage.completeness,
        RunCompleteness::Succeeded
    );
}

#[test]
fn unavailable_source_cannot_claim_observed_quality() {
    let mut capture = required_capture();
    capture.process_summary = RawSource::Unavailable(MetricQuality::Observed);

    let observation = parse_host_resource_v1(&capture);
    assert_eq!(observation.process.quality, MetricQuality::ParseFailed);
    assert!(observation.process.value.is_none());
}

#[test]
fn malformed_required_family_is_parse_failed_without_erasing_other_families() {
    let mut capture = required_capture();
    capture.load = RawSource::output("not-a-load-average\n");

    let observation = parse_host_resource_v1(&capture);
    assert_eq!(observation.load.quality, MetricQuality::ParseFailed);
    assert_eq!(observation.cpu.quality, MetricQuality::Observed);
    assert_eq!(observation.memory.quality, MetricQuality::Observed);
    assert_eq!(observation.disk_capacity.quality, MetricQuality::Observed);
    assert_eq!(observation.uptime.quality, MetricQuality::Observed);
    assert_eq!(observation.coverage.completeness, RunCompleteness::Partial);
}

#[test]
fn full_capture_parses_capacity_io_network_uptime_and_bounded_processes() {
    let observation = parse_host_resource_v1(&full_capture());
    assert_eq!(
        observation.coverage.completeness,
        RunCompleteness::Succeeded
    );
    assert_eq!(observation.coverage.optional_observed, 3);

    let filesystem = &observation.disk_capacity.value.as_ref().expect("capacity")[0];
    close(
        filesystem.allocatable_used_ratio.expect("capacity ratio"),
        400.0 / 900.0,
    );
    close(
        filesystem.inode_used_ratio.expect("inode ratio"),
        400.0 / 900.0,
    );

    let disk = &observation.disk_io.value.as_ref().expect("disk io")[0];
    let disk_metrics = disk.metrics.as_ref().expect("disk rate");
    close(disk_metrics.read_bytes_per_second, 10_240.0);
    close(disk_metrics.write_bytes_per_second, 15_360.0);
    close(disk_metrics.iops, 15.0);
    close(disk_metrics.util_pct, 2.0);

    let interface = &observation.network.value.as_ref().expect("network")[0];
    let network = interface.metrics.as_ref().expect("network rate");
    close(network.rx_bytes_per_second, 2_000.0);
    close(network.tx_bytes_per_second, 3_000.0);

    let process = observation.process.value.expect("process summary");
    assert_eq!(process.scanned, 120);
    assert_eq!(process.running, 3);
    assert_eq!(process.blocked, 2);
    assert_eq!(process.zombie, 1);
    assert!(!process.truncated);
}

#[test]
fn action_mapping_rejects_unknown_ids_instead_of_opening_a_command_channel() {
    let mut capture = HostResourceCapture::default();
    assert!(
        capture
            .set_action("user-provided-command", RawSource::output("ignored"))
            .is_err()
    );
    capture
        .set_action("memory", RawSource::output("MemTotal: 1 kB\n"))
        .expect("known fixed action");
}
