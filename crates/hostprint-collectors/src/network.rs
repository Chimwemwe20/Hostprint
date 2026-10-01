//! Interfaces, listening sockets, TCP state counts, routes and DNS.

use crate::util::{is_root, read_trimmed};
use crate::{CaptureContext, CollectError, Collected, Collector, Section};
use hostprint_model::{Dns, Interface, ListeningSocket, Network, PortRange, Route};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

pub struct NetworkCollector;

impl Collector for NetworkCollector {
    fn name(&self) -> &'static str {
        "network"
    }

    fn title(&self) -> &'static str {
        "Network"
    }

    fn collect(&self, ctx: &CaptureContext) -> Result<Collected, CollectError> {
        ctx.require_linux()?;
        let mut sockets = Vec::new();
        let mut readable = 0;
        for (file, protocol) in [("tcp", "tcp"), ("tcp6", "tcp"), ("udp", "udp"), ("udp6", "udp")] {
            if let Ok(contents) = std::fs::read_to_string(ctx.path(&format!("/proc/net/{file}"))) {
                readable += 1;
                sockets.extend(parse_socket_table(&contents).into_iter().map(|s| (protocol, s)));
            }
        }
        if readable == 0 {
            return Err(CollectError::Failed("cannot read /proc/net socket tables".into()));
        }

        let owners = socket_owners(ctx);
        let mut listening = BTreeSet::new();
        let mut tcp_states: BTreeMap<String, u64> = BTreeMap::new();
        let mut unowned = 0;
        for (protocol, s) in &sockets {
            let is_listener = match *protocol {
                "tcp" => s.state == TCP_LISTEN,
                _ => s.state == UDP_UNCONNECTED && s.remote_port == 0,
            };
            if !is_listener {
                if *protocol == "tcp" {
                    *tcp_states.entry(tcp_state_name(s.state).to_string()).or_default() += 1;
                }
                continue;
            }
            let owner = owners.get(&s.inode);
            if owner.is_none() {
                unowned += 1;
            }
            listening.insert(ListeningSocket {
                protocol: protocol.to_string(),
                address: s.local.to_string(),
                port: s.local_port,
                pid: owner.map(|o| o.0),
                process: owner.map(|o| o.1.clone()),
            });
        }

        let resolv = std::fs::read_to_string(ctx.path("/etc/resolv.conf")).unwrap_or_default();
        let mut dns = parse_resolv_conf(&resolv);
        if let Ok(upstream) = std::fs::read_to_string(ctx.path("/run/systemd/resolve/resolv.conf")) {
            let upstream = parse_resolv_conf(&upstream).nameservers;
            if upstream != dns.nameservers {
                dns.upstream_nameservers = upstream;
            }
        }
        let mut default_gateways = Vec::new();
        if let Ok(route) = std::fs::read_to_string(ctx.path("/proc/net/route")) {
            default_gateways.extend(parse_ipv4_default_routes(&route));
        }
        if let Ok(route) = std::fs::read_to_string(ctx.path("/proc/net/ipv6_route")) {
            default_gateways.extend(parse_ipv6_default_routes(&route));
        }
        default_gateways.sort();
        default_gateways.dedup();
        let ephemeral_ports =
            read_trimmed(&ctx.path("/proc/sys/net/ipv4/ip_local_port_range")).and_then(|s| parse_port_range(&s));

        let interfaces = interfaces(ctx);
        let listening: Vec<ListeningSocket> = listening.into_iter().collect();
        let summary = format!("{} listening sockets · {} interfaces", listening.len(), interfaces.len());
        let mut collected = Collected::new(Section::Network(Network {
            interfaces,
            listening,
            tcp_states,
            default_gateways,
            dns,
            ephemeral_ports,
        }))
        .summary(summary);
        if unowned > 0 && ctx.is_live() && !is_root() {
            collected = collected
                .note(format!("owning process unknown for {unowned} listening sockets (run as root for full details)"));
        }
        Ok(collected)
    }
}

const TCP_LISTEN: u8 = 0x0A;
const UDP_UNCONNECTED: u8 = 0x07;

fn tcp_state_name(state: u8) -> &'static str {
    match state {
        0x01 => "ESTABLISHED",
        0x02 => "SYN_SENT",
        0x03 => "SYN_RECV",
        0x04 => "FIN_WAIT1",
        0x05 => "FIN_WAIT2",
        0x06 => "TIME_WAIT",
        0x07 => "CLOSE",
        0x08 => "CLOSE_WAIT",
        0x09 => "LAST_ACK",
        0x0A => "LISTEN",
        0x0B => "CLOSING",
        0x0C => "NEW_SYN_RECV",
        _ => "UNKNOWN",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SocketEntry {
    pub local: IpAddr,
    pub local_port: u16,
    pub remote: IpAddr,
    pub remote_port: u16,
    pub state: u8,
    pub inode: u64,
}

/// Parses `/proc/net/{tcp,tcp6,udp,udp6}`.
pub(crate) fn parse_socket_table(contents: &str) -> Vec<SocketEntry> {
    contents
        .lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            let (local, local_port) = parse_endpoint(f.get(1)?)?;
            let (remote, remote_port) = parse_endpoint(f.get(2)?)?;
            Some(SocketEntry {
                local,
                local_port,
                remote,
                remote_port,
                state: u8::from_str_radix(f.get(3)?, 16).ok()?,
                inode: f.get(9)?.parse().ok()?,
            })
        })
        .collect()
}

/// `0100007F:1F90` → 127.0.0.1:8080. Addresses are printed as native-endian
/// 32-bit words, ports as plain hex.
fn parse_endpoint(field: &str) -> Option<(IpAddr, u16)> {
    let (addr, port) = field.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let words: Option<Vec<[u8; 4]>> = (0..addr.len() / 8)
        .map(|i| addr.get(i * 8..i * 8 + 8).and_then(|w| u32::from_str_radix(w, 16).ok()).map(u32::to_ne_bytes))
        .collect();
    let bytes: Vec<u8> = words?.concat();
    let ip = match bytes.len() {
        4 => IpAddr::V4(Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3])),
        16 => {
            let arr: [u8; 16] = bytes.try_into().ok()?;
            let v6 = Ipv6Addr::from(arr);
            // Show IPv4-mapped addresses (dual-stack sockets) in IPv4 form.
            match v6.to_ipv4_mapped() {
                Some(v4) => IpAddr::V4(v4),
                None => IpAddr::V6(v6),
            }
        }
        _ => return None,
    };
    Some((ip, port))
}

/// Maps socket inodes to the (lowest) PID and name of a process holding them.
fn socket_owners(ctx: &CaptureContext) -> HashMap<u64, (u32, String)> {
    let proc_dir = ctx.path("/proc");
    let mut pids: Vec<u32> = std::fs::read_dir(&proc_dir)
        .map(|entries| entries.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok()).collect())
        .unwrap_or_default();
    pids.sort_unstable();
    let mut owners = HashMap::new();
    for pid in pids {
        let dir = proc_dir.join(pid.to_string());
        let Ok(fds) = std::fs::read_dir(dir.join("fd")) else { continue };
        let mut name = None;
        for fd in fds.flatten() {
            let Ok(target) = std::fs::read_link(fd.path()) else { continue };
            let target = target.to_string_lossy();
            let Some(inode) =
                target.strip_prefix("socket:[").and_then(|t| t.strip_suffix(']')).and_then(|t| t.parse::<u64>().ok())
            else {
                continue;
            };
            owners.entry(inode).or_insert_with(|| {
                let name =
                    name.get_or_insert_with(|| read_trimmed(&dir.join("comm")).unwrap_or_else(|| pid.to_string()));
                (pid, name.clone())
            });
        }
    }
    owners
}

pub(crate) fn parse_resolv_conf(contents: &str) -> Dns {
    let mut dns = Dns::default();
    for line in contents.lines() {
        let line = line.split(['#', ';']).next().unwrap_or("").trim();
        let mut f = line.split_whitespace();
        match f.next() {
            Some("nameserver") => dns.nameservers.extend(f.next().map(str::to_string)),
            Some("search") | Some("domain") => dns.search = f.map(str::to_string).collect(),
            _ => {}
        }
    }
    dns
}

pub(crate) fn parse_ipv4_default_routes(contents: &str) -> Vec<Route> {
    contents
        .lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.get(1) != Some(&"00000000") || f.get(7) != Some(&"00000000") {
                return None;
            }
            let gw = u32::from_str_radix(f.get(2)?, 16).ok()?.to_ne_bytes();
            Some(Route { gateway: Ipv4Addr::from(gw).to_string(), interface: f[0].to_string() })
        })
        .collect()
}

pub(crate) fn parse_ipv6_default_routes(contents: &str) -> Vec<Route> {
    const ZERO: &str = "00000000000000000000000000000000";
    contents
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 10 || f[0] != ZERO || f[1] != "00" || f[4] == ZERO || f[9] == "lo" {
                return None;
            }
            let mut bytes = [0u8; 16];
            for (i, b) in bytes.iter_mut().enumerate() {
                *b = u8::from_str_radix(f[4].get(i * 2..i * 2 + 2)?, 16).ok()?;
            }
            Some(Route { gateway: Ipv6Addr::from(bytes).to_string(), interface: f[9].to_string() })
        })
        .collect()
}

pub(crate) fn parse_port_range(contents: &str) -> Option<PortRange> {
    let mut f = contents.split_whitespace().map(|p| p.parse::<u16>().ok());
    Some(PortRange { start: f.next()??, end: f.next()?? })
}

fn interfaces(ctx: &CaptureContext) -> Vec<Interface> {
    let net = ctx.path("/sys/class/net");
    let mut names: Vec<String> = std::fs::read_dir(&net)
        .map(|entries| entries.filter_map(|e| e.ok()?.file_name().into_string().ok()).collect())
        .unwrap_or_default();
    names.sort();
    let mut addresses = if ctx.is_live() { interface_addresses() } else { HashMap::new() };
    names
        .into_iter()
        .map(|name| {
            let dir = net.join(&name);
            let is_virtual =
                std::fs::read_link(&dir).map(|t| t.to_string_lossy().contains("/virtual/")).unwrap_or(false);
            let mut addrs = addresses.remove(&name).unwrap_or_default();
            addrs.sort();
            addrs.dedup();
            Interface {
                state: read_trimmed(&dir.join("operstate")),
                mac: read_trimmed(&dir.join("address")).filter(|m| m != "00:00:00:00:00:00"),
                mtu: read_trimmed(&dir.join("mtu")).and_then(|m| m.parse().ok()),
                addresses: addrs,
                is_virtual,
                name,
            }
        })
        .collect()
}

/// Interface addresses in CIDR notation, keyed by interface name.
#[cfg(target_os = "linux")]
fn interface_addresses() -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    // SAFETY: getifaddrs returns a linked list we only read and then free.
    // Address pointers are checked for null and cast according to sa_family.
    unsafe {
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut head) != 0 {
            return out;
        }
        let mut cur = head;
        while !cur.is_null() {
            let ifa = &*cur;
            cur = ifa.ifa_next;
            if ifa.ifa_addr.is_null() {
                continue;
            }
            let name = std::ffi::CStr::from_ptr(ifa.ifa_name).to_string_lossy().into_owned();
            let cidr = match i32::from((*ifa.ifa_addr).sa_family) {
                libc::AF_INET => {
                    let sa = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                    let ip = Ipv4Addr::from(u32::from_be(sa.sin_addr.s_addr));
                    let prefix = if ifa.ifa_netmask.is_null() {
                        32
                    } else {
                        let nm = &*(ifa.ifa_netmask as *const libc::sockaddr_in);
                        nm.sin_addr.s_addr.count_ones()
                    };
                    format!("{ip}/{prefix}")
                }
                libc::AF_INET6 => {
                    let sa = &*(ifa.ifa_addr as *const libc::sockaddr_in6);
                    let ip = Ipv6Addr::from(sa.sin6_addr.s6_addr);
                    let prefix = if ifa.ifa_netmask.is_null() {
                        128
                    } else {
                        let nm = &*(ifa.ifa_netmask as *const libc::sockaddr_in6);
                        nm.sin6_addr.s6_addr.iter().map(|b| b.count_ones()).sum::<u32>()
                    };
                    format!("{ip}/{prefix}")
                }
                _ => continue,
            };
            out.entry(name).or_default().push(cidr);
        }
        libc::freeifaddrs(head);
    }
    out
}

#[cfg(not(target_os = "linux"))]
fn interface_addresses() -> HashMap<String, Vec<String>> {
    HashMap::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tcp_tables() {
        let tcp = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 00000000:1538 00000000:0000 0A 00000000:00000000 00:00000000 00000000   999        0 23456 1 0000000000000000 100 0 0 10 0\n\
   1: 0100007F:1F90 0100007F:D431 01 00000000:00000000 00:00000000 00000000  1000        0 34567 1 0000000000000000 20 4 30 10 -1\n";
        let entries = parse_socket_table(tcp);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].local.to_string(), "0.0.0.0");
        assert_eq!(entries[0].local_port, 5432);
        assert_eq!(entries[0].state, TCP_LISTEN);
        assert_eq!(entries[0].inode, 23456);
        if cfg!(target_endian = "little") {
            assert_eq!(entries[1].local.to_string(), "127.0.0.1");
        }
        assert_eq!(entries[1].remote_port, 0xD431);
    }

    #[test]
    #[cfg(target_endian = "little")]
    fn parses_tcp6_endpoints() {
        let (ip, port) = parse_endpoint("00000000000000000000000001000000:0050").unwrap();
        assert_eq!((ip.to_string(), port), ("::1".to_string(), 80));
        let (ip, _) = parse_endpoint("0000000000000000FFFF00000100007F:0050").unwrap();
        assert_eq!(ip.to_string(), "127.0.0.1");
    }

    #[test]
    fn parses_resolv_conf() {
        let dns =
            parse_resolv_conf("# generated\nnameserver 127.0.0.53\noptions edns0\nsearch corp.example svc.local\n");
        assert_eq!(dns.nameservers, ["127.0.0.53"]);
        assert_eq!(dns.search, ["corp.example", "svc.local"]);
    }

    #[test]
    #[cfg(target_endian = "little")]
    fn parses_default_routes() {
        let route = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
eth0\t0001A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n";
        assert_eq!(
            parse_ipv4_default_routes(route),
            [Route { gateway: "192.168.1.1".into(), interface: "eth0".into() }]
        );
        let v6 = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000400 00000001 00000000 00000003 eth0\n\
00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 ffffffff 00000001 00000000 00200200 lo\n";
        assert_eq!(parse_ipv6_default_routes(v6), [Route { gateway: "fe80::1".into(), interface: "eth0".into() }]);
    }

    #[test]
    fn parses_port_range() {
        assert_eq!(parse_port_range("32768\t60999"), Some(PortRange { start: 32768, end: 60999 }));
    }
}
