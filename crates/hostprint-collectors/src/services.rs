//! systemd service units, via `systemctl`.

use crate::util::{run_command, which};
use crate::{CaptureContext, CollectError, Collected, Collector, Section};
use hostprint_model::Service;
use std::collections::HashMap;
use std::path::Path;

const SHOW_PROPERTIES: &str = "--property=Id,Type,NRestarts,Result,ActiveEnterTimestamp,MainPID";
/// Units per `systemctl show` call, keeping argument lists reasonable.
const SHOW_BATCH: usize = 200;

pub struct ServiceCollector;

impl Collector for ServiceCollector {
    fn name(&self) -> &'static str {
        "services"
    }

    fn title(&self) -> &'static str {
        "Services"
    }

    fn collect(&self, ctx: &CaptureContext) -> Result<Collected, CollectError> {
        #[cfg(target_os = "macos")]
        if ctx.is_live() {
            return crate::macos::live::services(ctx);
        }
        ctx.require_linux()?;
        if !Path::new("/run/systemd/system").exists() {
            return Err(CollectError::Unavailable("systemd is not running".into()));
        }
        let systemctl = which("systemctl").ok_or_else(|| CollectError::Unavailable("systemctl not found".into()))?;
        let out = run_command(
            &systemctl,
            &["list-units", "--type=service", "--all", "--no-legend", "--no-pager", "--plain", "--full"],
            None,
            ctx.command_timeout,
        )?;
        if !out.success {
            return Err(CollectError::Failed(format!("systemctl list-units: {}", out.error_line())));
        }
        let mut services = parse_list_units(&out.stdout);

        let mut details = HashMap::new();
        let names: Vec<String> = services.iter().map(|s| s.name.clone()).collect();
        let mut notes = Vec::new();
        for batch in names.chunks(SHOW_BATCH) {
            let mut args = vec!["show", "--no-pager", SHOW_PROPERTIES, "--"];
            args.extend(batch.iter().map(String::as_str));
            match run_command(&systemctl, &args, None, ctx.command_timeout) {
                Ok(out) if out.success => details.extend(parse_show(&out.stdout)),
                Ok(out) => notes.push(format!("systemctl show: {}", out.error_line())),
                Err(e) => notes.push(format!("systemctl show: {e}")),
            }
        }
        for service in &mut services {
            if let Some(props) = details.get(&service.name) {
                apply_properties(service, props);
            }
        }

        let failed = services.iter().filter(|s| s.active_state == "failed").count();
        let summary = format!("{} services · {} failed", services.len(), failed);
        let mut collected = Collected::new(Section::Services(services)).summary(summary);
        collected.notes = notes;
        Ok(collected)
    }
}

/// Parses `systemctl list-units --plain --no-legend` output.
pub(crate) fn parse_list_units(stdout: &str) -> Vec<Service> {
    let mut services: Vec<Service> = stdout
        .lines()
        .filter_map(|line| {
            let line = line.trim_start().trim_start_matches(['●', '*']).trim_start();
            let mut f = line.split_whitespace();
            let name = f.next()?.to_string();
            if !name.ends_with(".service") {
                return None;
            }
            let load_state = f.next()?.to_string();
            let active_state = f.next()?.to_string();
            let sub_state = f.next()?.to_string();
            let description = f.collect::<Vec<_>>().join(" ");
            Some(Service {
                name,
                description: (!description.is_empty()).then_some(description),
                load_state,
                active_state,
                sub_state,
                service_type: None,
                restarts: None,
                result: None,
                active_since: None,
                main_pid: None,
            })
        })
        .collect();
    services.sort_by(|a, b| a.name.cmp(&b.name));
    services
}

/// Parses `systemctl show` output: `Key=Value` blocks separated by blank lines.
pub(crate) fn parse_show(stdout: &str) -> HashMap<String, HashMap<String, String>> {
    let mut out = HashMap::new();
    for block in stdout.split("\n\n") {
        let props: HashMap<String, String> =
            block.lines().filter_map(|l| l.split_once('=').map(|(k, v)| (k.to_string(), v.to_string()))).collect();
        if let Some(id) = props.get("Id").cloned() {
            out.insert(id, props);
        }
    }
    out
}

fn apply_properties(service: &mut Service, props: &HashMap<String, String>) {
    let get = |k: &str| props.get(k).map(|v| v.trim()).filter(|v| !v.is_empty() && *v != "n/a");
    service.service_type = get("Type").map(str::to_string);
    service.restarts = get("NRestarts").and_then(|v| v.parse().ok());
    service.result = get("Result").map(str::to_string);
    service.active_since = get("ActiveEnterTimestamp").map(str::to_string);
    service.main_pid = get("MainPID").and_then(|v| v.parse().ok()).filter(|pid| *pid != 0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_units_and_properties() {
        let list = "ssh.service      loaded active   running SSH server\n\
                    ● redis.service  loaded failed   failed  Advanced key-value store\n\
                    apt-daily.service loaded inactive dead   Daily apt download activities\n\
                    foo.socket loaded active listening Not a service\n";
        let mut services = parse_list_units(list);
        assert_eq!(services.len(), 3);
        assert_eq!(services[0].name, "apt-daily.service");
        assert_eq!(services[1].name, "redis.service");
        assert_eq!(services[1].active_state, "failed");
        assert_eq!(services[1].description.as_deref(), Some("Advanced key-value store"));

        let show = "Id=redis.service\nType=notify\nNRestarts=17\nResult=exit-code\nActiveEnterTimestamp=\nMainPID=0\n\n\
                    Id=ssh.service\nType=notify\nNRestarts=0\nResult=success\nActiveEnterTimestamp=Wed 2026-10-01 09:00:00 UTC\nMainPID=812\n";
        let props = parse_show(show);
        apply_properties(&mut services[1], &props["redis.service"]);
        assert_eq!(services[1].restarts, Some(17));
        assert_eq!(services[1].active_since, None);
        assert_eq!(services[1].main_pid, None);
        apply_properties(&mut services[2], &props["ssh.service"]);
        assert_eq!(services[2].main_pid, Some(812));
        assert_eq!(services[2].active_since.as_deref(), Some("Wed 2026-10-01 09:00:00 UTC"));
    }
}
