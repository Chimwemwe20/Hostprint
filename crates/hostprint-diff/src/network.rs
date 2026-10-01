use crate::{set_delta, Category, Change, DiffOptions, Significance, Significance::*};
use hostprint_model::{Interface, Network, PortRange};
use std::collections::{BTreeMap, BTreeSet};

/// Used when a snapshot did not record the kernel's ephemeral port range.
const DEFAULT_EPHEMERAL: PortRange = PortRange { start: 32768, end: 60999 };

pub(crate) fn compare(a: &Network, b: &Network, opts: &DiffOptions, out: &mut Vec<Change>) {
    listeners(a, b, opts, out);
    interfaces(&a.interfaces, &b.interfaces, out);
    routes_and_dns(a, b, out);
    tcp_states(a, b, out);
}

fn change(sig: Significance, rule: &str, key: impl Into<String>, subject: impl Into<String>) -> Change {
    Change::new(sig, Category::Network, rule, key, subject)
}

#[derive(Default)]
struct Listener {
    addresses: BTreeSet<String>,
    processes: BTreeSet<String>,
}

impl Listener {
    fn describe(&self) -> String {
        let who = if self.processes.is_empty() {
            "unknown process".to_string()
        } else {
            self.processes.iter().cloned().collect::<Vec<_>>().join(", ")
        };
        let addrs: Vec<&str> = self.addresses.iter().map(String::as_str).collect();
        format!("{who} on {}", addrs.join(", "))
    }

    fn is_exposed(&self) -> bool {
        self.addresses.iter().any(|a| a == "0.0.0.0" || a == "::")
    }
}

fn listeners(a: &Network, b: &Network, opts: &DiffOptions, out: &mut Vec<Change>) {
    let group = |n: &Network| {
        let mut map: BTreeMap<(String, u16), Listener> = BTreeMap::new();
        for s in n.listening.iter().filter(|s| !opts.ignore_ports.contains(&s.port)) {
            let l = map.entry((s.protocol.clone(), s.port)).or_default();
            l.addresses.insert(s.address.clone());
            l.processes.extend(s.process.clone());
        }
        map
    };
    let (la, lb) = (group(a), group(b));
    let ephemeral = b.ephemeral_ports.or(a.ephemeral_ports).unwrap_or(DEFAULT_EPHEMERAL);
    let keys: BTreeSet<&(String, u16)> = la.keys().chain(lb.keys()).collect();

    for key in keys {
        let (proto, port) = key;
        let subject = format!("{proto}/{port}");
        let k = |field: &str| format!("network/listening/{proto}/{port}/{field}");
        let transient = ephemeral.contains(*port);
        let tcp = proto == "tcp";
        match (la.get(key), lb.get(key)) {
            (Some(x), None) => {
                let sig = if transient {
                    Info
                } else if tcp {
                    High
                } else {
                    Low
                };
                out.push(
                    change(sig, "network.listener_removed", k("listening"), subject)
                        .field("listening")
                        .removed(x.describe()),
                );
            }
            (None, Some(y)) => {
                let sig = if transient {
                    Info
                } else if tcp {
                    Medium
                } else {
                    Low
                };
                out.push(
                    change(sig, "network.listener_added", k("listening"), subject)
                        .field("listening")
                        .added(y.describe()),
                );
            }
            (Some(x), Some(y)) => {
                // Owner comparison needs both sides to know the owner.
                if !x.processes.is_empty() && !y.processes.is_empty() && x.processes != y.processes {
                    let join = |s: &BTreeSet<String>| s.iter().cloned().collect::<Vec<_>>().join(", ");
                    out.push(
                        change(
                            if transient { Info } else { Medium },
                            "network.listener_owner",
                            k("process"),
                            subject.clone(),
                        )
                        .field("process")
                        .values(join(&x.processes), join(&y.processes)),
                    );
                }
                if x.addresses != y.addresses {
                    let (sig, rule) = if transient {
                        (Info, "network.listener_address")
                    } else if y.is_exposed() && !x.is_exposed() {
                        (Medium, "network.listener_exposed")
                    } else {
                        (Low, "network.listener_address")
                    };
                    let join = |s: &BTreeSet<String>| s.iter().cloned().collect::<Vec<_>>().join(", ");
                    out.push(
                        change(sig, rule, k("address"), subject)
                            .field("bind address")
                            .values(join(&x.addresses), join(&y.addresses)),
                    );
                }
            }
            (None, None) => unreachable!(),
        }
    }
}

fn interfaces(a: &[Interface], b: &[Interface], out: &mut Vec<Change>) {
    let index = |v: &[Interface]| v.iter().map(|i| (i.name.clone(), i.clone())).collect::<BTreeMap<_, _>>();
    let (ia, ib) = (index(a), index(b));
    let k = |name: &str, field: &str| format!("network/interfaces/{name}/{field}");

    for (name, x) in &ia {
        let Some(y) = ib.get(name) else {
            let sig = if x.is_virtual { Info } else { Medium };
            out.push(
                change(sig, "network.interface_removed", k(name, "present"), name)
                    .field("interface")
                    .removed(x.addresses.join(", ")),
            );
            continue;
        };
        let virtual_iface = x.is_virtual || y.is_virtual;
        if x.state != y.state {
            let (sa, sb) = (x.state.as_deref().unwrap_or("unknown"), y.state.as_deref().unwrap_or("unknown"));
            let (sig, rule) = if sa == "up" && !virtual_iface {
                (High, "network.interface_down")
            } else if virtual_iface {
                (Info, "network.interface_state")
            } else {
                (Low, "network.interface_state")
            };
            out.push(change(sig, rule, k(name, "state"), name).field("state").values(sa, sb));
        }
        let routable = |i: &Interface| -> BTreeSet<String> {
            i.addresses.iter().filter(|a| !a.starts_with("fe80:")).cloned().collect()
        };
        let (ra, rb) = (routable(x), routable(y));
        if ra != rb {
            let added: Vec<&str> = rb.difference(&ra).map(String::as_str).collect();
            let removed: Vec<&str> = ra.difference(&rb).map(String::as_str).collect();
            let join = |s: &BTreeSet<String>| {
                if s.is_empty() {
                    "none".to_string()
                } else {
                    s.iter().cloned().collect::<Vec<_>>().join(", ")
                }
            };
            out.push(
                change(
                    if virtual_iface { Info } else { Medium },
                    "network.interface_address",
                    k(name, "addresses"),
                    name,
                )
                .field("addresses")
                .values(join(&ra), join(&rb))
                .delta(set_delta(added, removed)),
            );
        }
        if x.mtu != y.mtu {
            let mtu = |m: Option<u32>| m.map(|m| m.to_string()).unwrap_or_else(|| "unknown".into());
            out.push(
                change(if virtual_iface { Info } else { Low }, "network.interface_mtu", k(name, "mtu"), name)
                    .field("MTU")
                    .values(mtu(x.mtu), mtu(y.mtu)),
            );
        }
        if x.mac != y.mac {
            let mac = |m: &Option<String>| m.clone().unwrap_or_else(|| "none".into());
            out.push(
                change(if virtual_iface { Info } else { Low }, "network.interface_mac", k(name, "mac"), name)
                    .field("MAC")
                    .values(mac(&x.mac), mac(&y.mac)),
            );
        }
    }
    for (name, y) in &ib {
        if !ia.contains_key(name) {
            let sig = if y.is_virtual { Info } else { Low };
            out.push(
                change(sig, "network.interface_added", k(name, "present"), name)
                    .field("interface")
                    .added(y.addresses.join(", ")),
            );
        }
    }
}

fn routes_and_dns(a: &Network, b: &Network, out: &mut Vec<Change>) {
    let routes = |n: &Network| {
        let v: Vec<String> = n.default_gateways.iter().map(|r| format!("{} via {}", r.gateway, r.interface)).collect();
        if v.is_empty() {
            "none".to_string()
        } else {
            v.join(", ")
        }
    };
    if a.default_gateways != b.default_gateways {
        out.push(change(Medium, "network.gateway", "network/gateway", "Default gateway").values(routes(a), routes(b)));
    }
    let list = |v: &[String]| if v.is_empty() { "none".to_string() } else { v.join(", ") };
    if a.dns.nameservers != b.dns.nameservers {
        out.push(
            change(Medium, "network.dns", "network/dns/nameservers", "DNS servers")
                .values(list(&a.dns.nameservers), list(&b.dns.nameservers)),
        );
    }
    if a.dns.upstream_nameservers != b.dns.upstream_nameservers {
        out.push(
            change(Medium, "network.dns", "network/dns/upstream", "Upstream DNS servers")
                .values(list(&a.dns.upstream_nameservers), list(&b.dns.upstream_nameservers)),
        );
    }
    if a.dns.search != b.dns.search {
        out.push(
            change(Low, "network.dns_search", "network/dns/search", "DNS search domains")
                .values(list(&a.dns.search), list(&b.dns.search)),
        );
    }
}

fn tcp_states(a: &Network, b: &Network, out: &mut Vec<Change>) {
    let get = |n: &Network, s: &str| n.tcp_states.get(s).copied().unwrap_or(0);
    // (state, minimum count after, growth factor, significance)
    let surges = [
        ("CLOSE_WAIT", 50, 3, Medium),
        ("SYN_SENT", 50, 3, Medium),
        ("SYN_RECV", 100, 3, Medium),
        ("TIME_WAIT", 1000, 3, Low),
        ("ESTABLISHED", 100, 3, Low),
    ];
    for (state, min, factor, sig) in surges {
        let (x, y) = (get(a, state), get(b, state));
        if y >= min && y >= factor * x.max(1) {
            out.push(
                change(sig, "network.tcp_state", format!("network/tcp/{state}"), format!("TCP {state}"))
                    .field("sockets")
                    .values(x.to_string(), y.to_string()),
            );
        }
    }
    let (x, y) = (get(a, "ESTABLISHED"), get(b, "ESTABLISHED"));
    if x >= 20 && y * 3 <= x {
        out.push(
            change(Low, "network.tcp_state", "network/tcp/ESTABLISHED", "TCP ESTABLISHED")
                .field("sockets")
                .values(x.to_string(), y.to_string()),
        );
    }
}

#[cfg(test)]
mod tests {
    use crate::testing::*;
    use crate::{diff, Change, DiffOptions, Significance, Significance::*};
    use hostprint_model::Snapshot;

    fn changes(edit: impl FnOnce(&mut hostprint_model::Network)) -> Vec<Change> {
        let a = baseline();
        let mut b: Snapshot = later(&a, 600);
        edit(b.network.as_mut().unwrap());
        diff(&a, &b, &DiffOptions::default()).changes
    }

    fn rules(changes: &[Change]) -> Vec<(&str, Significance)> {
        changes.iter().map(|c| (c.rule.as_str(), c.significance)).collect()
    }

    #[test]
    fn listener_changes() {
        let c = changes(|n| {
            n.listening.retain(|l| l.port != 5432);
            n.listening.push(listener("tcp", "0.0.0.0", 9090, "node"));
            n.listening.push(listener("tcp", "0.0.0.0", 45123, "node"));
            n.listening.push(listener("udp", "0.0.0.0", 5353, "avahi-daemon"));
            n.listening.iter_mut().find(|l| l.port == 6379).unwrap().address = "0.0.0.0".into();
        });
        assert_eq!(
            rules(&c),
            [
                ("network.listener_removed", High),
                ("network.listener_exposed", Medium),
                ("network.listener_added", Medium),
                ("network.listener_added", Low),
                ("network.listener_added", Info),
            ]
        );
        assert_eq!(c[0].subject, "tcp/5432");
        assert_eq!(c[0].before.as_deref(), Some("postgres on 127.0.0.1"));
    }

    #[test]
    fn unknown_owner_is_not_an_owner_change() {
        let c = changes(|n| {
            for l in &mut n.listening {
                l.process = None;
                l.pid = None;
            }
        });
        assert_eq!(c, []);
    }

    #[test]
    fn interfaces_routes_dns_and_tcp() {
        let c = changes(|n| {
            n.interfaces[0].state = Some("down".into());
            n.interfaces[0].addresses = vec![];
            n.interfaces.push(hostprint_model::Interface {
                name: "veth12ab".into(),
                state: Some("up".into()),
                mac: None,
                mtu: Some(1500),
                addresses: vec![],
                is_virtual: true,
            });
            n.dns.nameservers = vec!["1.1.1.1".into()];
            n.tcp_states.insert("CLOSE_WAIT".into(), 400);
        });
        assert_eq!(
            rules(&c),
            [
                ("network.interface_down", High),
                ("network.dns", Medium),
                ("network.tcp_state", Medium),
                ("network.interface_address", Medium),
                ("network.interface_added", Info),
            ]
        );
    }

    #[test]
    fn ignored_ports() {
        let a = baseline();
        let mut b = later(&a, 60);
        b.network.as_mut().unwrap().listening.retain(|l| l.port != 22);
        let opts = DiffOptions { ignore_ports: vec![22], ..Default::default() };
        assert_eq!(diff(&a, &b, &opts).changes, []);
    }
}
