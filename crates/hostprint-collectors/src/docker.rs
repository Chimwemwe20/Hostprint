//! Docker containers, read from the Engine API over its Unix socket.
//!
//! Speaking HTTP/1.0 over the socket directly keeps Hostprint free of an async
//! runtime and of a dependency on the `docker` CLI being installed.

use crate::{CaptureContext, CollectError, Collected, Collector, Section};
use hostprint_model::{Container, Docker};
use serde_json::Value;
use std::path::PathBuf;

/// Containers inspected concurrently.
const PARALLELISM: usize = 8;

pub struct DockerCollector;

impl Collector for DockerCollector {
    fn name(&self) -> &'static str {
        "docker"
    }

    fn title(&self) -> &'static str {
        "Docker"
    }

    fn collect(&self, ctx: &CaptureContext) -> Result<Collected, CollectError> {
        imp::collect(ctx)
    }
}

/// Candidate socket paths, most specific first.
fn socket_candidates() -> Result<Vec<PathBuf>, CollectError> {
    if let Ok(host) = std::env::var("DOCKER_HOST") {
        return match host.strip_prefix("unix://") {
            Some(path) => Ok(vec![PathBuf::from(path)]),
            None => Err(CollectError::Unavailable(format!(
                "DOCKER_HOST={host} is not a Unix socket; only unix:// is supported"
            ))),
        };
    }
    let mut candidates = vec![PathBuf::from("/var/run/docker.sock")];
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        candidates.push(PathBuf::from(runtime).join("docker.sock"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(".docker/run/docker.sock"));
    }
    Ok(candidates)
}

/// Builds a container from its `/containers/json` summary and inspect output.
pub(crate) fn container_from_api(summary: &Value, inspect: &Value) -> Container {
    let str_at = |v: &Value, ptr: &str| v.pointer(ptr).and_then(Value::as_str).map(str::to_string);
    let id = str_at(summary, "/Id").unwrap_or_default();
    let name = str_at(inspect, "/Name")
        .or_else(|| str_at(summary, "/Names/0"))
        .unwrap_or_else(|| id.clone())
        .trim_start_matches('/')
        .to_string();
    let state =
        str_at(inspect, "/State/Status").or_else(|| str_at(summary, "/State")).unwrap_or_else(|| "unknown".into());
    let running = state == "running" || state == "restarting" || state == "paused";

    let mut ports: Vec<String> = summary
        .get("Ports")
        .and_then(Value::as_array)
        .map(|ports| {
            ports
                .iter()
                .map(|p| {
                    let private = p.get("PrivatePort").and_then(Value::as_u64).unwrap_or(0);
                    let proto = p.get("Type").and_then(Value::as_str).unwrap_or("tcp");
                    match p.get("PublicPort").and_then(Value::as_u64) {
                        Some(public) => {
                            let ip = p.get("IP").and_then(Value::as_str).unwrap_or("0.0.0.0");
                            let ip = if ip == "::" { "0.0.0.0" } else { ip };
                            format!("{ip}:{public}->{private}/{proto}")
                        }
                        None => format!("{private}/{proto}"),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    ports.sort();
    ports.dedup();

    let label = |key: &str| summary.get("Labels").and_then(|l| l.get(key)).and_then(Value::as_str).map(str::to_string);
    Container {
        id: id.chars().take(12).collect(),
        name,
        image: str_at(inspect, "/Config/Image").or_else(|| str_at(summary, "/Image")).unwrap_or_default(),
        image_id: str_at(inspect, "/Image")
            .or_else(|| str_at(summary, "/ImageID"))
            .map(|i| i.trim_start_matches("sha256:").chars().take(12).collect()),
        status: str_at(summary, "/Status"),
        health: str_at(inspect, "/State/Health/Status").filter(|h| !h.is_empty() && h != "none"),
        restart_count: inspect.get("RestartCount").and_then(Value::as_u64).unwrap_or(0) as u32,
        exit_code: (!running).then(|| inspect.pointer("/State/ExitCode").and_then(Value::as_i64)).flatten(),
        oom_killed: inspect.pointer("/State/OOMKilled").and_then(Value::as_bool).unwrap_or(false),
        started_at: str_at(inspect, "/State/StartedAt").filter(|s| !s.starts_with("0001-")),
        ports,
        memory_bytes: None,
        memory_limit_bytes: None,
        compose_project: label("com.docker.compose.project"),
        compose_service: label("com.docker.compose.service"),
        state,
    }
}

/// Memory usage as `docker stats` reports it: usage minus reclaimable page cache.
pub(crate) fn memory_from_stats(stats: &Value) -> (Option<u64>, Option<u64>) {
    let mem = stats.get("memory_stats");
    let usage = mem.and_then(|m| m.get("usage")).and_then(Value::as_u64);
    let cache = mem.and_then(|m| m.get("stats")).and_then(|s| {
        ["inactive_file", "total_inactive_file", "cache"].iter().find_map(|k| s.get(*k).and_then(Value::as_u64))
    });
    let limit = mem.and_then(|m| m.get("limit")).and_then(Value::as_u64);
    (usage.map(|u| u.saturating_sub(cache.unwrap_or(0))), limit)
}

/// The Engine API client, shared with the log collector.
#[cfg(unix)]
pub(crate) mod imp {
    use super::*;
    use std::io::{self, Read, Write};
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::time::Duration;

    /// The first Docker socket that exists.
    pub(crate) fn socket() -> Result<PathBuf, CollectError> {
        socket_candidates()?
            .into_iter()
            .find(|p| p.exists())
            .ok_or_else(|| CollectError::Unavailable("Docker socket not found".into()))
    }

    pub(super) fn collect(ctx: &CaptureContext) -> Result<Collected, CollectError> {
        let socket = socket()?;
        let timeout = ctx.command_timeout;
        let containers = get_json(&socket, "/containers/json?all=1", timeout).map_err(|e| connect_error(&socket, e))?;
        let summaries = containers.as_array().cloned().unwrap_or_default();
        let engine_version = get_json(&socket, "/version", timeout)
            .ok()
            .and_then(|v| v.get("Version").and_then(Value::as_str).map(str::to_string));

        let mut notes = Vec::new();
        let chunk = summaries.len().div_ceil(PARALLELISM).max(1);
        let results: Vec<Result<Container, String>> = std::thread::scope(|scope| {
            let handles: Vec<_> = summaries
                .chunks(chunk)
                .map(|batch| {
                    let socket = &socket;
                    scope.spawn(move || batch.iter().map(|s| inspect(socket, s, timeout)).collect::<Vec<_>>())
                })
                .collect();
            handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
        });
        let mut list = Vec::with_capacity(results.len());
        for result in results {
            match result {
                Ok(c) => list.push(c),
                Err(e) => notes.push(e),
            }
        }
        list.sort_by(|a, b| a.name.cmp(&b.name));

        let running = list.iter().filter(|c| c.state == "running").count();
        let summary = format!("{} containers · {} running", list.len(), running);
        let mut collected =
            Collected::new(Section::Docker(Docker { engine_version, containers: list })).summary(summary);
        collected.notes = notes;
        Ok(collected)
    }

    fn inspect(socket: &Path, summary: &Value, timeout: Duration) -> Result<Container, String> {
        let id = summary.get("Id").and_then(Value::as_str).unwrap_or_default();
        let details = get_json(socket, &format!("/containers/{id}/json"), timeout)
            .map_err(|e| format!("inspect {}: {e}", &id[..id.len().min(12)]))?;
        let mut container = container_from_api(summary, &details);
        if container.state == "running" {
            // one-shot skips the second sample needed for CPU percentages,
            // which keeps this call fast.
            if let Ok(stats) = get_json(socket, &format!("/containers/{id}/stats?stream=false&one-shot=true"), timeout)
            {
                let (usage, limit) = memory_from_stats(&stats);
                container.memory_bytes = usage;
                container.memory_limit_bytes = limit;
            }
        }
        Ok(container)
    }

    pub(crate) fn connect_error(socket: &Path, err: io::Error) -> CollectError {
        let path = socket.display();
        match err.kind() {
            io::ErrorKind::PermissionDenied => CollectError::Failed(format!(
                "permission denied on {path} (add your user to the docker group or run as root)"
            )),
            io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound => {
                CollectError::Failed(format!("Docker daemon not reachable at {path}"))
            }
            _ => CollectError::Failed(format!("Docker API error at {path}: {err}")),
        }
    }

    pub(crate) fn get_json(socket: &Path, path: &str, timeout: Duration) -> io::Result<Value> {
        let body = get_bytes(socket, path, timeout)?;
        serde_json::from_slice(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// GET returning the raw body of a 2xx response.
    pub(crate) fn get_bytes(socket: &Path, path: &str, timeout: Duration) -> io::Result<Vec<u8>> {
        let (status, body) = http_get(socket, path, timeout)?;
        if !(200..300).contains(&status) {
            let message = serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_string))
                .unwrap_or_else(|| format!("HTTP {status}"));
            return Err(io::Error::other(message));
        }
        Ok(body)
    }

    fn http_get(socket: &Path, path: &str, timeout: Duration) -> io::Result<(u16, Vec<u8>)> {
        let mut stream = UnixStream::connect(socket)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        write!(
            stream,
            "GET {path} HTTP/1.0\r\nHost: docker\r\nUser-Agent: hostprint/{}\r\n\r\n",
            env!("CARGO_PKG_VERSION")
        )?;
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw)?;
        super::parse_http_response(&raw)
    }
}

#[cfg(not(unix))]
mod imp {
    use super::*;

    pub(super) fn collect(_ctx: &CaptureContext) -> Result<Collected, CollectError> {
        let _ = socket_candidates;
        Err(CollectError::Unavailable(format!("not supported on {} yet", std::env::consts::OS)))
    }
}

/// Splits a raw HTTP/1.x response into status and body, decoding chunked
/// transfer encoding if the server used it.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn parse_http_response(raw: &[u8]) -> std::io::Result<(u16, Vec<u8>)> {
    use std::io::{Error, ErrorKind};
    let invalid = |msg: &str| Error::new(ErrorKind::InvalidData, msg.to_string());
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| invalid("malformed HTTP response"))?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let body = &raw[split + 4..];
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| invalid("missing HTTP status"))?;
    let chunked = head.lines().any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });
    if !chunked {
        return Ok((status, body.to_vec()));
    }
    let mut out = Vec::new();
    let mut rest = body;
    loop {
        let line_end = rest.windows(2).position(|w| w == b"\r\n").ok_or_else(|| invalid("bad chunk"))?;
        let size_str = String::from_utf8_lossy(&rest[..line_end]);
        let size = usize::from_str_radix(size_str.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| invalid("bad chunk size"))?;
        rest = &rest[line_end + 2..];
        if size == 0 {
            break;
        }
        if rest.len() < size {
            return Err(invalid("truncated chunk"));
        }
        out.extend_from_slice(&rest[..size]);
        rest = rest.get(size + 2..).unwrap_or(&[]);
    }
    Ok((status, out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builds_container_from_api_output() {
        let summary = json!({
            "Id": "4f1d2c3b4a5e6f7081928374655647382910",
            "Names": ["/redis"],
            "Image": "redis:7",
            "ImageID": "sha256:aaaabbbbccccdddd",
            "State": "restarting",
            "Status": "Restarting (1) 3 seconds ago",
            "Ports": [
                {"IP": "0.0.0.0", "PrivatePort": 6379, "PublicPort": 6379, "Type": "tcp"},
                {"IP": "::", "PrivatePort": 6379, "PublicPort": 6379, "Type": "tcp"}
            ],
            "Labels": {"com.docker.compose.project": "demo", "com.docker.compose.service": "redis"}
        });
        let inspect = json!({
            "Name": "/redis",
            "RestartCount": 17,
            "Image": "sha256:1234567890abcdef",
            "Config": {"Image": "redis:7-alpine"},
            "State": {"Status": "restarting", "ExitCode": 1, "OOMKilled": false,
                      "StartedAt": "2026-10-01T14:29:58Z", "Health": {"Status": "unhealthy"}}
        });
        let c = container_from_api(&summary, &inspect);
        assert_eq!(c.id, "4f1d2c3b4a5e");
        assert_eq!(c.name, "redis");
        assert_eq!(c.image, "redis:7-alpine");
        assert_eq!(c.image_id.as_deref(), Some("1234567890ab"));
        assert_eq!(c.state, "restarting");
        assert_eq!(c.health.as_deref(), Some("unhealthy"));
        assert_eq!(c.restart_count, 17);
        assert_eq!(c.exit_code, None, "exit code is only meaningful once stopped");
        assert_eq!(c.ports, ["0.0.0.0:6379->6379/tcp"]);
        assert_eq!(c.compose_service.as_deref(), Some("redis"));
    }

    #[test]
    fn computes_memory_like_docker_stats() {
        let stats = json!({"memory_stats": {"usage": 500, "limit": 1000, "stats": {"inactive_file": 120}}});
        assert_eq!(memory_from_stats(&stats), (Some(380), Some(1000)));
    }

    #[test]
    fn parses_http_responses() {
        let plain = b"HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n[1,2]";
        assert_eq!(parse_http_response(plain).unwrap(), (200, b"[1,2]".to_vec()));
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\n[1,\r\n2\r\n2]\r\n0\r\n\r\n";
        assert_eq!(parse_http_response(chunked).unwrap(), (200, b"[1,2]".to_vec()));
        let err = b"HTTP/1.0 404 Not Found\r\n\r\n{\"message\":\"no such container\"}";
        assert_eq!(parse_http_response(err).unwrap().0, 404);
    }
}
