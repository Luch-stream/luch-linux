//! Asking every address of the LAN (PROTOCOL.md 1b): for networks that drop multicast (hotel and guest Wi-Fi, some
//! mesh systems), a receiver is still a TCP port. Every address of the interface's subnet, nearest to ours first
//! and the neighbours the kernel already knows before them, gets a connection to the control port and, where one
//! is accepted, the real hello. Bounded: so many at once, so many in all, so long.

use std::{future::Future, net::Ipv4Addr, time::Duration};

use futures_util::{stream, Stream, StreamExt};

/// At most this many addresses per interface: a /22. A bigger subnet (a hotel's /16) is cut to the /22 around us,
/// where a DHCP server that hands out addresses in order most likely put the TV too.
pub const MAX_HOSTS: usize = 1024;
/// Connections in flight at once: an address with no host costs a whole timeout (the ARP request goes unanswered),
/// so this is what makes the sweep fast, and what keeps it from flooding a small router.
pub const IN_FLIGHT: usize = 128;
/// No host on the LAN takes longer than this to accept; a missing one is given up after it.
pub const CONNECT_TIMEOUT: Duration = Duration::from_millis(400);
/// A receiver answers hello within 2 s (section 3).
pub const WELCOME_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Iface {
    pub name: String,
    pub index: u32,
    pub addr: Ipv4Addr,
    pub prefix: u8,
    pub flags: u32,
}

/// Names of interfaces that are never a LAN with a TV on it: VPNs and other tunnels, and the bridges of
/// containers and virtual machines on this computer.
const NOT_A_LAN: [&str; 14] = [
    "tun",
    "tap",
    "wg",
    "ppp",
    "tailscale",
    "zt",
    "nordlynx",
    "proton",
    "docker",
    "br-",
    "veth",
    "virbr",
    "lxc",
    "podman",
];

/// Section 2: never through a VPN. Point-to-point links (tun, wg, ppp) and loopback are left out by their flags,
/// tunnels without that flag (a tap, a Tailscale interface) by name.
pub fn scannable(i: &Iface) -> bool {
    let up = i.flags & libc::IFF_UP as u32 != 0 && i.flags & libc::IFF_RUNNING as u32 != 0;
    let odd = i.flags & (libc::IFF_LOOPBACK | libc::IFF_POINTOPOINT) as u32 != 0;
    up && !odd && i.prefix >= 8 && !NOT_A_LAN.iter().any(|p| i.name.starts_with(p))
}

/// The addresses of `i`'s subnet (cut to MAX_HOSTS around ours), nearest to ours first, without ours, the network
/// and the broadcast address.
pub fn targets(i: &Iface) -> Vec<Ipv4Addr> {
    let min_prefix = 32 - MAX_HOSTS.trailing_zeros() as u8;
    let prefix = i.prefix.max(min_prefix);
    if prefix >= 31 {
        return Vec::new();
    }
    let me = u32::from(i.addr);
    let mask = u32::MAX << (32 - prefix);
    let (net, last) = (me & mask, (me & mask) | !mask);
    let mut hosts: Vec<u32> = (net + 1..last).filter(|&h| h != me).collect();
    hosts.sort_by_key(|&h| (h.abs_diff(me), h));
    hosts.into_iter().map(Ipv4Addr::from).collect()
}

/// Complete entries of the kernel's neighbour table on `ifname`: hosts known to be up, worth asking first.
pub fn neighbours(arp: &str, ifname: &str) -> Vec<Ipv4Addr> {
    arp.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<_> = l.split_whitespace().collect();
            (f.len() >= 6 && f[5] == ifname && f[2] != "0x0")
                .then(|| f[0].parse().ok())
                .flatten()
        })
        .collect()
}

/// The order of the sweep: the known neighbours, then the rest of the subnet.
pub fn plan(i: &Iface, arp: &str) -> Vec<Ipv4Addr> {
    let all = targets(i);
    let mut first: Vec<Ipv4Addr> = neighbours(arp, &i.name)
        .into_iter()
        .filter(|a| all.contains(a))
        .collect();
    first.dedup();
    let rest: Vec<Ipv4Addr> = all.into_iter().filter(|a| !first.contains(a)).collect();
    first.extend(rest);
    first
}

/// This computer's IPv4 interfaces.
pub fn interfaces() -> Vec<Iface> {
    let mut out = Vec::new();
    let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills ifap with a list that freeifaddrs releases below; nothing outlives it.
    if unsafe { libc::getifaddrs(&mut ifap) } != 0 {
        return out;
    }
    let mut p = ifap;
    while !p.is_null() {
        // SAFETY: p is a node of the list getifaddrs returned.
        let ifa = unsafe { &*p };
        p = ifa.ifa_next;
        if ifa.ifa_addr.is_null() || ifa.ifa_netmask.is_null() {
            continue;
        }
        // SAFETY: ifa_addr is non-null; its family says which sockaddr it is.
        if i32::from(unsafe { (*ifa.ifa_addr).sa_family }) != libc::AF_INET {
            continue;
        }
        // SAFETY: AF_INET addresses and masks are sockaddr_in.
        let (addr, mask) = unsafe {
            (
                (*(ifa.ifa_addr as *const libc::sockaddr_in))
                    .sin_addr
                    .s_addr,
                (*(ifa.ifa_netmask as *const libc::sockaddr_in))
                    .sin_addr
                    .s_addr,
            )
        };
        // SAFETY: ifa_name is a NUL-terminated string owned by the list.
        let name = unsafe { std::ffi::CStr::from_ptr(ifa.ifa_name) }
            .to_string_lossy()
            .into_owned();
        // SAFETY: a plain lookup by a valid C string.
        let index = unsafe { libc::if_nametoindex(ifa.ifa_name) };
        out.push(Iface {
            name,
            index,
            addr: Ipv4Addr::from(u32::from_be(addr)),
            prefix: u32::from_be(mask).leading_ones() as u8,
            flags: ifa.ifa_flags,
        });
    }
    // SAFETY: the list from getifaddrs, freed once.
    unsafe { libc::freeifaddrs(ifap) };
    out
}

/// The interface to reach `ip` from: the LAN whose subnet holds it, else the interface of the main default route
/// (a receiver behind a router), never a tunnel.
pub fn iface_for(ip: Ipv4Addr, ifaces: &[Iface], lan: &str) -> Option<Iface> {
    let on_link = |i: &&&Iface| {
        let mask = if i.prefix == 0 {
            0
        } else {
            u32::MAX << (32 - i.prefix)
        };
        u32::from(i.addr) & mask == u32::from(ip) & mask
    };
    let usable: Vec<&Iface> = ifaces
        .iter()
        .filter(|i| scannable(i) || i.flags & libc::IFF_LOOPBACK as u32 != 0 && ip.is_loopback())
        .collect();
    usable
        .iter()
        .find(on_link)
        .or_else(|| usable.iter().find(|i| i.name == lan))
        .map(|i| (*i).clone())
}

/// Runs `probe` on every target, at most `limit` at once, and yields what it finds as soon as it does.
pub fn sweep<A, T, F, Fut>(targets: Vec<A>, limit: usize, probe: F) -> impl Stream<Item = T>
where
    F: FnMut(A) -> Fut,
    Fut: Future<Output = Option<T>>,
{
    stream::iter(targets)
        .map(probe)
        .buffer_unordered(limit)
        .filter_map(std::future::ready)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    const UP: u32 = (libc::IFF_UP | libc::IFF_RUNNING) as u32;

    fn wlan(addr: [u8; 4], prefix: u8) -> Iface {
        Iface {
            name: "wlan0".into(),
            index: 3,
            addr: Ipv4Addr::from(addr),
            prefix,
            flags: UP,
        }
    }

    #[test]
    fn vpns_and_virtual_bridges_are_never_scanned() {
        assert!(scannable(&wlan([192, 168, 1, 10], 24)));
        let named = |n: &str, flags| Iface {
            name: n.into(),
            flags,
            ..wlan([10, 8, 0, 2], 24)
        };
        assert!(!scannable(&named("wg0", UP | libc::IFF_POINTOPOINT as u32)));
        assert!(!scannable(&named(
            "anything",
            UP | libc::IFF_POINTOPOINT as u32
        )));
        assert!(!scannable(&named("tailscale0", UP)));
        assert!(!scannable(&named("tap0", UP)));
        assert!(!scannable(&named("docker0", UP)));
        assert!(!scannable(&named("lo", UP | libc::IFF_LOOPBACK as u32)));
        assert!(
            !scannable(&named("eth0", libc::IFF_UP as u32)),
            "no carrier"
        );
        assert!(scannable(&named("enp3s0", UP)));
    }

    #[test]
    fn a_24_is_every_other_host_nearest_first() {
        let t = targets(&wlan([192, 168, 1, 10], 24));
        assert_eq!(t.len(), 253);
        assert_eq!(
            &t[..4],
            &[
                Ipv4Addr::new(192, 168, 1, 9),
                Ipv4Addr::new(192, 168, 1, 11),
                Ipv4Addr::new(192, 168, 1, 8),
                Ipv4Addr::new(192, 168, 1, 12)
            ]
        );
        assert!(!t.contains(&Ipv4Addr::new(192, 168, 1, 10)));
        assert!(!t.contains(&Ipv4Addr::new(192, 168, 1, 0)));
        assert!(!t.contains(&Ipv4Addr::new(192, 168, 1, 255)));
    }

    #[test]
    fn a_big_subnet_is_cut_to_the_22_around_us() {
        let t = targets(&wlan([10, 20, 130, 7], 16));
        assert_eq!(t.len(), MAX_HOSTS - 3);
        assert!(t
            .iter()
            .all(|a| a.octets()[2] >= 128 && a.octets()[2] < 132));
        assert!(targets(&wlan([10, 0, 0, 1], 31)).is_empty());
        assert_eq!(
            targets(&wlan([10, 0, 0, 1], 30)),
            vec![Ipv4Addr::new(10, 0, 0, 2)]
        );
    }

    #[test]
    fn known_neighbours_go_first() {
        let arp =
            "IP address       HW type     Flags       HW address            Mask     Device\n\
                   192.168.1.200    0x1         0x2         aa:bb:cc:dd:ee:01     *        wlan0\n\
                   192.168.1.201    0x1         0x0         00:00:00:00:00:00     *        wlan0\n\
                   10.8.0.1         0x1         0x2         aa:bb:cc:dd:ee:02     *        wg0\n\
                   192.168.1.1      0x1         0x2         aa:bb:cc:dd:ee:03     *        wlan0\n";
        let p = plan(&wlan([192, 168, 1, 10], 24), arp);
        assert_eq!(
            &p[..3],
            &[
                Ipv4Addr::new(192, 168, 1, 200),
                Ipv4Addr::new(192, 168, 1, 1),
                Ipv4Addr::new(192, 168, 1, 9)
            ]
        );
        assert_eq!(p.len(), 253);
    }

    #[test]
    fn the_interface_for_an_address() {
        let ifs = vec![
            wlan([192, 168, 1, 10], 24),
            Iface {
                name: "wg0".into(),
                flags: UP | libc::IFF_POINTOPOINT as u32,
                ..wlan([10, 8, 0, 2], 8)
            },
            Iface {
                name: "eth0".into(),
                ..wlan([172, 16, 0, 5], 16)
            },
        ];
        let at = |ip: [u8; 4]| iface_for(Ipv4Addr::from(ip), &ifs, "wlan0").map(|i| i.name);
        assert_eq!(at([192, 168, 1, 20]).as_deref(), Some("wlan0"));
        assert_eq!(at([172, 16, 9, 9]).as_deref(), Some("eth0"));
        // Inside the VPN's subnet, but a receiver is never reached through it: the LAN is used instead.
        assert_eq!(at([10, 1, 2, 3]).as_deref(), Some("wlan0"));
    }

    #[test]
    fn this_computer_has_interfaces() {
        assert!(interfaces()
            .iter()
            .any(|i| i.addr.is_loopback() && !scannable(i)));
    }

    #[tokio::test(start_paused = true)]
    async fn the_sweep_is_bounded_and_yields_as_it_goes() {
        let (now, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (n, p) = (now.clone(), peak.clone());
        let started = tokio::time::Instant::now();
        let found: Vec<u32> = sweep((0..1000u32).collect(), 100, move |a| {
            let (n, p) = (n.clone(), p.clone());
            async move {
                let in_flight = n.fetch_add(1, Ordering::SeqCst) + 1;
                p.fetch_max(in_flight, Ordering::SeqCst);
                // Most addresses are empty and cost a whole timeout; a few answer.
                tokio::time::sleep(if a % 250 == 7 {
                    Duration::from_millis(5)
                } else {
                    CONNECT_TIMEOUT
                })
                .await;
                n.fetch_sub(1, Ordering::SeqCst);
                (a % 250 == 7).then_some(a)
            }
        })
        .collect()
        .await;
        let mut found = found;
        found.sort();
        assert_eq!(found, vec![7, 257, 507, 757]);
        assert_eq!(peak.load(Ordering::SeqCst), 100);
        // 1000 addresses, 100 at a time, 400 ms each: 4 s, not 400.
        assert!(
            started.elapsed() <= Duration::from_millis(4100),
            "{:?}",
            started.elapsed()
        );
    }
}
