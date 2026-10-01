//! A realistic baseline snapshot for rule tests. Tests clone it, change one
//! thing, and assert on the resulting changes.

use chrono::{DateTime, Duration, Utc};
use hostprint_model::*;
use std::collections::BTreeMap;

pub const MIB: u64 = 1024 * 1024;
pub const GIB: u64 = 1024 * MIB;

pub fn at(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_790_000_000 + secs, 0).unwrap()
}

pub fn process(pid: u32, name: &str, memory: u64, started: DateTime<Utc>) -> Process {
    Process {
        pid,
        ppid: 1,
        name: name.into(),
        exe: Some(format!("/usr/bin/{name}")),
        cmdline: Some(name.into()),
        user: Some("app".into()),
        uid: Some(1000),
        state: "S".into(),
        cpu_percent: Some(0.0),
        memory_bytes: memory,
        threads: 1,
        started_at: Some(started),
    }
}

pub fn listener(protocol: &str, address: &str, port: u16, process: &str) -> ListeningSocket {
    ListeningSocket {
        protocol: protocol.into(),
        address: address.into(),
        port,
        pid: Some(100),
        process: Some(process.into()),
    }
}

pub fn service(name: &str, active: &str, sub: &str) -> Service {
    Service {
        name: name.into(),
        description: None,
        load_state: "loaded".into(),
        active_state: active.into(),
        sub_state: sub.into(),
        service_type: Some("simple".into()),
        restarts: Some(0),
        result: Some("success".into()),
        active_since: Some("Thu 2026-10-01 04:40:00 UTC".into()),
        main_pid: Some(500),
    }
}

pub fn container(name: &str, image: &str) -> Container {
    Container {
        id: format!("{name:0<12}"),
        name: name.into(),
        image: image.into(),
        image_id: Some("aaaaaaaaaaaa".into()),
        state: "running".into(),
        status: Some("Up 4 hours (healthy)".into()),
        health: Some("healthy".into()),
        restart_count: 0,
        exit_code: None,
        oom_killed: false,
        started_at: Some("2026-10-01T04:41:00Z".into()),
        ports: vec![],
        memory_bytes: Some(100 * MIB),
        memory_limit_bytes: Some(8 * GIB),
        compose_project: Some("demo".into()),
        compose_service: Some(name.into()),
    }
}

pub fn env(name: &str, value: &str) -> EnvVar {
    EnvVar { name: name.into(), source: "process".into(), value: value.into(), redacted: false, fingerprint: None }
}

pub fn secret(name: &str, fingerprint: &str) -> EnvVar {
    EnvVar {
        name: name.into(),
        source: "process".into(),
        value: REDACTED.into(),
        redacted: true,
        fingerprint: Some(fingerprint.into()),
    }
}

fn report(name: &str) -> CollectorReport {
    CollectorReport {
        name: name.into(),
        status: CollectorStatus::Ok,
        duration_ms: 1,
        summary: None,
        message: None,
        notes: vec![],
    }
}

pub fn set_status(s: &mut Snapshot, collector: &str, status: CollectorStatus, message: &str) {
    let r = s.capture.collectors.iter_mut().find(|r| r.name == collector).unwrap();
    r.status = status;
    r.message = Some(message.into());
}

/// Captured at `at(0)` on a machine booted an hour earlier.
pub fn baseline() -> Snapshot {
    let boot = at(-3600);
    Snapshot {
        schema_version: SCHEMA_VERSION,
        id: "snap_a".into(),
        name: "healthy".into(),
        captured_at: at(0),
        capture: CaptureInfo {
            hostprint_version: "0.1.0".into(),
            duration_ms: 900,
            user: Some("app".into()),
            uid: Some(1000),
            elevated: false,
            working_dir: Some("/srv/app".into()),
            collectors: [
                "system",
                "resources",
                "processes",
                "network",
                "services",
                "docker",
                "git",
                "runtimes",
                "environment",
                "files",
            ]
            .iter()
            .map(|n| report(n))
            .collect(),
        },
        host: Some(Host {
            hostname: "web-1".into(),
            os: Some(OsRelease {
                id: Some("ubuntu".into()),
                name: Some("Ubuntu".into()),
                version_id: Some("24.04".into()),
                pretty_name: Some("Ubuntu 24.04.1 LTS".into()),
            }),
            kernel: Some("6.8.0-45-generic".into()),
            architecture: "x86_64".into(),
            boot_time: Some(boot),
            uptime_seconds: Some(3600),
            timezone: Some("Etc/UTC".into()),
            hardware: Some("QEMU Standard PC".into()),
            container: None,
        }),
        resources: Some(Resources {
            cpu: Cpu {
                model: Some("AMD EPYC".into()),
                logical_cores: 4,
                physical_cores: Some(2),
                usage_percent: Some(12.0),
                iowait_percent: Some(0.5),
                steal_percent: Some(0.0),
            },
            load: Some(LoadAverage { one: 0.5, five: 0.4, fifteen: 0.3 }),
            memory: Memory {
                total_bytes: 8 * GIB,
                available_bytes: 5 * GIB + 400 * MIB,
                used_bytes: 2 * GIB + 624 * MIB,
                free_bytes: 3 * GIB,
            },
            swap: Swap { total_bytes: 2 * GIB, used_bytes: 0, free_bytes: 2 * GIB },
            pressure: Some(Pressure {
                cpu_some_avg60: Some(0.5),
                memory_some_avg60: Some(0.0),
                memory_full_avg60: Some(0.0),
                io_some_avg60: Some(1.0),
                io_full_avg60: Some(0.5),
            }),
            disks: vec![Disk {
                mount_point: "/".into(),
                device: "/dev/sda1".into(),
                filesystem: "ext4".into(),
                read_only: false,
                unresponsive: false,
                total_bytes: Some(100 * GIB),
                used_bytes: Some(40 * GIB),
                available_bytes: Some(55 * GIB),
                inodes_total: Some(6_000_000),
                inodes_free: Some(5_000_000),
            }],
        }),
        processes: Some(Processes {
            kernel_threads: 90,
            list: vec![
                process(1, "systemd", 12 * MIB, boot),
                process(400, "postgres", 200 * MIB, boot),
                process(401, "postgres", 50 * MIB, boot),
                process(500, "node", 300 * MIB, at(-3000)),
                process(600, "redis-server", 20 * MIB, boot),
                process(700, "sshd", 8 * MIB, boot),
            ],
        }),
        network: Some(Network {
            interfaces: vec![
                Interface {
                    name: "eth0".into(),
                    state: Some("up".into()),
                    mac: Some("52:54:00:12:34:56".into()),
                    mtu: Some(1500),
                    addresses: vec!["10.0.0.5/24".into(), "fe80::5054:ff:fe12:3456/64".into()],
                    is_virtual: false,
                },
                Interface {
                    name: "lo".into(),
                    state: Some("unknown".into()),
                    mac: None,
                    mtu: Some(65536),
                    addresses: vec!["127.0.0.1/8".into()],
                    is_virtual: true,
                },
            ],
            listening: vec![
                listener("tcp", "0.0.0.0", 22, "sshd"),
                listener("tcp", "0.0.0.0", 3000, "node"),
                listener("tcp", "127.0.0.1", 5432, "postgres"),
                listener("tcp", "127.0.0.1", 6379, "redis-server"),
            ],
            tcp_states: BTreeMap::from([("ESTABLISHED".into(), 40), ("TIME_WAIT".into(), 120)]),
            default_gateways: vec![Route { gateway: "10.0.0.1".into(), interface: "eth0".into() }],
            dns: Dns { nameservers: vec!["10.0.0.2".into()], search: vec![], upstream_nameservers: vec![] },
            ephemeral_ports: Some(PortRange { start: 32768, end: 60999 }),
        }),
        services: Some(vec![
            service("nginx.service", "active", "running"),
            service("postgresql.service", "active", "running"),
            Service { service_type: Some("oneshot".into()), ..service("apt-daily.service", "inactive", "dead") },
        ]),
        docker: Some(Docker {
            engine_version: Some("27.3.1".into()),
            containers: vec![container("api", "demo/api:1.4"), container("redis", "redis:7-alpine")],
        }),
        git: Some(Git {
            root: "/srv/app".into(),
            branch: Some("main".into()),
            commit: Some("b921cc1000000000000000000000000000000000".into()),
            commit_subject: Some("Tune connection pool".into()),
            commit_time: Some("2026-09-30T18:00:00Z".into()),
            describe: None,
            remote: Some("git@github.com:org/app.git".into()),
            dirty: false,
            staged: 0,
            modified: 0,
            untracked: 0,
            changed_paths: vec![],
        }),
        runtimes: Some(vec![
            Runtime { name: "node".into(), version: Some("22.8.0".into()), path: "/usr/bin/node".into() },
            Runtime { name: "python3".into(), version: Some("3.12.3".into()), path: "/usr/bin/python3".into() },
        ]),
        environment: Some(Environment {
            fingerprint_key_id: "key1".into(),
            variables: vec![
                env("DATABASE_POOL_SIZE", "10"),
                secret("JWT_SECRET", "1111111111111111"),
                env("PATH", "/usr/local/bin:/usr/bin:/bin"),
                env("PWD", "/srv/app"),
            ],
        }),
        files: Some(vec![FileFingerprint {
            path: "/etc/nginx/nginx.conf".into(),
            exists: true,
            size: Some(1500),
            modified: Some(at(-86_400)),
            sha256: Some("8d2740000000000000000000000000000000000000000000000000000000ffff".into()),
            mode: Some("0644".into()),
            uid: Some(0),
            gid: Some(0),
            error: None,
        }]),
    }
}

/// A second capture of the baseline machine `secs` later, as an unchanged
/// machine would produce it: new snapshot identity, uptime advanced.
pub fn later(base: &Snapshot, secs: i64) -> Snapshot {
    let mut s = base.clone();
    s.id = "snap_b".into();
    s.name = "incident".into();
    s.captured_at = base.captured_at + Duration::seconds(secs);
    if let Some(h) = &mut s.host {
        h.uptime_seconds = h.uptime_seconds.map(|u| u + secs as u64);
    }
    s
}
