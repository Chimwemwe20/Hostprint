//! Small but complete snapshots for TUI tests.

use chrono::{DateTime, Utc};
use hostprint_model::*;

pub fn at(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_790_000_000 + secs, 0).unwrap()
}

fn report(name: &str, status: CollectorStatus) -> CollectorReport {
    CollectorReport { name: name.into(), status, duration_ms: 1, summary: None, message: None, notes: vec![] }
}

pub fn container(name: &str, state: &str, health: &str, restarts: u32) -> Container {
    Container {
        id: format!("{name:0<12}"),
        name: name.into(),
        image: format!("{name}:1"),
        image_id: Some("aaaaaaaaaaaa".into()),
        state: state.into(),
        status: None,
        health: Some(health.into()),
        restart_count: restarts,
        exit_code: None,
        oom_killed: false,
        started_at: Some("2026-10-01T09:00:00Z".into()),
        ports: vec![],
        memory_bytes: Some(64 << 20),
        memory_limit_bytes: Some(8 << 30),
        compose_project: None,
        compose_service: None,
    }
}

pub fn service(name: &str, active: &str, sub: &str, restarts: u32) -> Service {
    Service {
        name: name.into(),
        description: None,
        load_state: "loaded".into(),
        active_state: active.into(),
        sub_state: sub.into(),
        service_type: Some("simple".into()),
        restarts: Some(restarts),
        result: None,
        active_since: Some("Thu 2026-10-01 09:00:00 UTC".into()),
        main_pid: None,
    }
}

/// A healthy machine captured `secs` after the fixture epoch.
pub fn snapshot(name: &str, secs: i64) -> Snapshot {
    Snapshot {
        schema_version: SCHEMA_VERSION,
        id: format!("snap_{name}"),
        name: name.into(),
        captured_at: at(secs),
        capture: CaptureInfo {
            hostprint_version: "0.1.0".into(),
            duration_ms: 300,
            user: Some("deploy".into()),
            uid: Some(1000),
            elevated: false,
            working_dir: None,
            remote: None,
            collectors: ["system", "resources", "processes", "services", "docker", "git"]
                .iter()
                .map(|n| report(n, CollectorStatus::Ok))
                .chain([report("logs", CollectorStatus::Skipped)])
                .collect(),
        },
        host: Some(Host {
            hostname: "web-1".into(),
            os: None,
            kernel: Some("6.8.0".into()),
            architecture: "x86_64".into(),
            boot_time: Some(at(-3600)),
            uptime_seconds: Some(3600),
            timezone: None,
            hardware: None,
            container: None,
        }),
        resources: Some(Resources {
            cpu: Cpu {
                model: None,
                logical_cores: 4,
                physical_cores: None,
                usage_percent: Some(38.0),
                iowait_percent: None,
                steal_percent: None,
            },
            load: Some(LoadAverage { one: 0.42, five: 0.5, fifteen: 0.6 }),
            memory: Memory { total_bytes: 8 << 30, available_bytes: 5 << 30, used_bytes: 3 << 30, free_bytes: 4 << 30 },
            swap: Swap { total_bytes: 0, used_bytes: 0, free_bytes: 0 },
            pressure: None,
            disks: vec![Disk {
                mount_point: "/".into(),
                device: "/dev/sda1".into(),
                filesystem: "ext4".into(),
                read_only: false,
                unresponsive: false,
                total_bytes: Some(100 << 30),
                used_bytes: Some(54 << 30),
                available_bytes: Some(46 << 30),
                inodes_total: None,
                inodes_free: None,
            }],
        }),
        processes: Some(Processes {
            kernel_threads: 10,
            list: vec![Process {
                pid: 812,
                ppid: 1,
                name: "redis-server".into(),
                exe: Some("/usr/bin/redis-server".into()),
                cmdline: Some("redis-server *:6379".into()),
                user: Some("redis".into()),
                uid: Some(999),
                state: "S".into(),
                cpu_percent: Some(0.5),
                memory_bytes: 12 << 20,
                threads: 4,
                started_at: Some(at(-3000)),
            }],
        }),
        network: None,
        services: Some(vec![service("nginx.service", "active", "running", 0)]),
        docker: Some(Docker {
            engine_version: Some("27.3.1".into()),
            containers: vec![container("api", "running", "healthy", 0), container("redis", "running", "healthy", 0)],
        }),
        git: None,
        runtimes: None,
        environment: None,
        files: None,
        logs: None,
    }
}

/// The same machine with redis crash-looping and nginx failed.
pub fn broken(name: &str, secs: i64) -> Snapshot {
    let mut s = snapshot(name, secs);
    let redis = &mut s.docker.as_mut().unwrap().containers[1];
    redis.state = "restarting".into();
    redis.health = Some("unhealthy".into());
    redis.restart_count = 17;
    s.services = Some(vec![service("nginx.service", "failed", "failed", 3)]);
    s
}
