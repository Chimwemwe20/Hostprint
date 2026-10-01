//! Versions of language runtimes and common server software on `PATH`.

use crate::util::{run_command, which};
use crate::{CaptureContext, CollectError, Collected, Collector, Section};
use hostprint_model::Runtime;

struct Probe {
    name: &'static str,
    binary: &'static str,
    args: &'static [&'static str],
    /// Line of the output that carries the version, when there are several.
    marker: Option<&'static str>,
}

const fn probe(name: &'static str, binary: &'static str, args: &'static [&'static str]) -> Probe {
    Probe { name, binary, args, marker: None }
}

const PROBES: &[Probe] = &[
    probe("node", "node", &["--version"]),
    probe("deno", "deno", &["--version"]),
    probe("bun", "bun", &["--version"]),
    probe("python3", "python3", &["--version"]),
    probe("ruby", "ruby", &["--version"]),
    probe("php", "php", &["--version"]),
    probe("java", "java", &["-version"]),
    probe("dotnet", "dotnet", &["--version"]),
    probe("go", "go", &["version"]),
    probe("rustc", "rustc", &["--version"]),
    Probe { name: "elixir", binary: "elixir", args: &["--version"], marker: Some("Elixir") },
    probe("erlang", "erl", &["-noshell", "-eval", "io:format(\"~s\", [erlang:system_info(otp_release)]), halt()."]),
    probe("docker", "docker", &["--version"]),
    probe("psql", "psql", &["--version"]),
    probe("postgres", "postgres", &["--version"]),
    probe("mysql", "mysql", &["--version"]),
    probe("redis-server", "redis-server", &["--version"]),
    probe("nginx", "nginx", &["-v"]),
    probe("openssl", "openssl", &["version"]),
    probe("git", "git", &["--version"]),
];

pub struct RuntimeCollector;

impl Collector for RuntimeCollector {
    fn name(&self) -> &'static str {
        "runtimes"
    }

    fn title(&self) -> &'static str {
        "Runtimes"
    }

    fn collect(&self, ctx: &CaptureContext) -> Result<Collected, CollectError> {
        let found: Vec<(&Probe, std::path::PathBuf)> =
            PROBES.iter().filter_map(|p| which(p.binary).map(|path| (p, path))).collect();
        let results: Vec<(Runtime, Option<String>)> = std::thread::scope(|scope| {
            let handles: Vec<_> = found
                .iter()
                .map(|(probe, path)| {
                    scope.spawn(move || {
                        let (version, problem) = match run_command(path, probe.args, None, ctx.command_timeout) {
                            Ok(out) => {
                                let text = format!("{}\n{}", out.stdout, out.stderr);
                                match parse_version(&text, probe.marker) {
                                    Some(v) => (Some(v), None),
                                    None => (None, Some(format!("{}: unrecognised version output", probe.name))),
                                }
                            }
                            Err(e) => (None, Some(format!("{}: {e}", probe.name))),
                        };
                        let runtime = Runtime {
                            name: probe.name.to_string(),
                            version,
                            path: path.to_string_lossy().into_owned(),
                        };
                        (runtime, problem)
                    })
                })
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });

        let mut runtimes = Vec::with_capacity(results.len());
        let mut notes = Vec::new();
        for (runtime, problem) in results {
            runtimes.push(runtime);
            notes.extend(problem);
        }
        runtimes.sort_by(|a, b| a.name.cmp(&b.name));
        let summary = format!("{} found", runtimes.len());
        let mut collected = Collected::new(Section::Runtimes(runtimes)).summary(summary);
        collected.notes = notes;
        Ok(collected)
    }
}

/// Extracts the first version number from tool output: a dotted number such
/// as `22.9.0` (with an optional `-suffix`), or a bare number if no dotted one
/// exists (`erl` prints just `27`).
pub(crate) fn parse_version(output: &str, marker: Option<&str>) -> Option<String> {
    let text = match marker {
        Some(m) => output.lines().find(|l| l.contains(m))?,
        None => output,
    };
    let chars: Vec<char> = text.chars().collect();
    let mut bare = None;
    for start in 0..chars.len() {
        let prev = start.checked_sub(1).map(|i| chars[i]);
        if !chars[start].is_ascii_digit() || prev.is_some_and(|c| c.is_ascii_digit() || c == '.') {
            continue;
        }
        let mut end = start;
        let mut dots = 0;
        loop {
            while end < chars.len() && chars[end].is_ascii_digit() {
                end += 1;
            }
            if end + 1 < chars.len() && chars[end] == '.' && chars[end + 1].is_ascii_digit() {
                dots += 1;
                end += 1;
            } else {
                break;
            }
        }
        if dots == 0 {
            bare.get_or_insert_with(|| chars[start..end].iter().collect::<String>());
            continue;
        }
        if end + 1 < chars.len() && matches!(chars[end], '-' | '+') && chars[end + 1].is_ascii_alphanumeric() {
            end += 1;
            while end < chars.len() && (chars[end].is_ascii_alphanumeric() || matches!(chars[end], '.' | '-' | '~')) {
                end += 1;
            }
        }
        return Some(chars[start..end].iter().collect());
    }
    bare
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_version_outputs() {
        let cases = [
            ("v22.9.0", "22.9.0"),
            ("Python 3.12.3", "3.12.3"),
            ("openjdk version \"21.0.4\" 2024-07-16\nOpenJDK Runtime Environment", "21.0.4"),
            ("go version go1.23.1 linux/amd64", "1.23.1"),
            ("rustc 1.81.0 (eeb90cda1 2024-09-04)", "1.81.0"),
            ("rustc 1.83.0-nightly (abc 2024-10-01)", "1.83.0-nightly"),
            ("Docker version 27.3.1, build ce12230", "27.3.1"),
            ("psql (PostgreSQL) 16.4 (Ubuntu 16.4-0ubuntu0.24.04.2)", "16.4"),
            ("Redis server v=7.2.5 sha=00000000:0 malloc=jemalloc-5.3.0 bits=64", "7.2.5"),
            ("nginx version: nginx/1.24.0 (Ubuntu)", "1.24.0"),
            ("OpenSSL 3.0.13 30 Jan 2024 (Library: OpenSSL 3.0.13 30 Jan 2024)", "3.0.13"),
            ("mysql  Ver 8.0.39-0ubuntu0.24.04.2 for Linux on x86_64", "8.0.39-0ubuntu0.24.04.2"),
            ("27", "27"),
        ];
        for (output, expected) in cases {
            assert_eq!(parse_version(output, None).as_deref(), Some(expected), "{output}");
        }
        let elixir = "Erlang/OTP 27 [erts-15.0] [source] [64-bit]\n\nElixir 1.18.4 (compiled with Erlang/OTP 27)";
        assert_eq!(parse_version(elixir, Some("Elixir")).as_deref(), Some("1.18.4"));
        assert_eq!(parse_version("no digits here", None), None);
    }
}
