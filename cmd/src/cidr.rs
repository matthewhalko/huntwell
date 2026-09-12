//! Address matching for API key allowlists.
//!
//! Small on purpose: an allowlist is a handful of entries checked once per
//! request, so this does the bit comparison by hand rather than taking on a
//! dependency for it.

use std::net::IpAddr;

/// One allowlist entry: an address and how many of its leading bits matter.
/// A bare address (no `/n`) is an exact match on the whole address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    addr: IpAddr,
    prefix_len: u8,
}

/// IPv4-mapped IPv6 (`::ffff:203.0.113.7`) is the same host as its IPv4 form,
/// and which one arrives depends on how the socket was opened. Comparing
/// without collapsing that would silently reject a legitimate client.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        v4 => v4,
    }
}

fn width(ip: &IpAddr) -> u8 {
    match ip {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    }
}

impl Cidr {
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("empty entry".to_string());
        }
        match text.split_once('/') {
            None => {
                let addr = canonical(
                    text.parse::<IpAddr>()
                        .map_err(|_| format!("{text:?} is not an IP address"))?,
                );
                Ok(Self {
                    prefix_len: width(&addr),
                    addr,
                })
            }
            Some((addr, bits)) => {
                // Keep the family exactly as written here: canonicalizing
                // would change what the prefix length means.
                let addr: IpAddr = addr
                    .trim()
                    .parse()
                    .map_err(|_| format!("{addr:?} is not an IP address"))?;
                let prefix_len: u8 = bits
                    .trim()
                    .parse()
                    .map_err(|_| format!("{bits:?} is not a prefix length"))?;
                if prefix_len > width(&addr) {
                    return Err(format!(
                        "/{prefix_len} is too long for {addr} (max /{})",
                        width(&addr)
                    ));
                }
                Ok(Self { addr, prefix_len })
            }
        }
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        match (canonical(self.addr), canonical(ip)) {
            (IpAddr::V4(net), IpAddr::V4(host)) => {
                prefix_eq(&net.octets(), &host.octets(), self.prefix_len)
            }
            (IpAddr::V6(net), IpAddr::V6(host)) => {
                prefix_eq(&net.octets(), &host.octets(), self.prefix_len)
            }
            // An IPv4 rule never authorizes an IPv6 client, or the reverse.
            _ => false,
        }
    }
}

fn prefix_eq(net: &[u8], host: &[u8], bits: u8) -> bool {
    let whole = (bits / 8) as usize;
    if net[..whole] != host[..whole] {
        return false;
    }
    let leftover = bits % 8;
    if leftover == 0 {
        return true;
    }
    let mask = 0xFFu8 << (8 - leftover);
    (net[whole] & mask) == (host[whole] & mask)
}

/// Parses a comma- or whitespace-separated allowlist. An empty string is not an
/// error: it means "no restriction".
pub fn parse_list(text: &str) -> Result<Vec<Cidr>, String> {
    text.split([',', ' ', '\t', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(Cidr::parse)
        .collect()
}

/// True when the list places no restriction, or when some entry covers `ip`.
pub fn allows(list: &[Cidr], ip: IpAddr) -> bool {
    list.is_empty() || list.iter().any(|c| c.contains(ip))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn a_bare_address_matches_only_itself() {
        let list = parse_list("203.0.113.7").unwrap();
        assert!(allows(&list, ip("203.0.113.7")));
        assert!(!allows(&list, ip("203.0.113.8")));
        assert!(!allows(&list, ip("::1")));
    }

    #[test]
    fn prefixes_match_on_bit_boundaries() {
        let list = parse_list("10.0.0.0/24").unwrap();
        assert!(allows(&list, ip("10.0.0.1")));
        assert!(allows(&list, ip("10.0.0.255")));
        assert!(!allows(&list, ip("10.0.1.1")));

        // A prefix that does not land on a byte boundary.
        let list = parse_list("192.168.8.0/22").unwrap();
        assert!(allows(&list, ip("192.168.8.1")));
        assert!(allows(&list, ip("192.168.11.254")));
        assert!(!allows(&list, ip("192.168.12.1")));
    }

    #[test]
    fn ipv6_and_mapped_ipv4_are_the_same_host() {
        let list = parse_list("203.0.113.7").unwrap();
        // What a dual-stack socket may report for an IPv4 client.
        assert!(allows(&list, ip("::ffff:203.0.113.7")));

        let list = parse_list("2001:db8::/32").unwrap();
        assert!(allows(&list, ip("2001:db8:1234::1")));
        assert!(!allows(&list, ip("2001:db9::1")));
    }

    #[test]
    fn an_empty_list_allows_everything() {
        let list = parse_list("   ").unwrap();
        assert!(list.is_empty());
        assert!(allows(&list, ip("203.0.113.7")));
        assert!(allows(&list, ip("::1")));
    }

    #[test]
    fn several_entries_in_one_string() {
        let list = parse_list("127.0.0.1, 10.0.0.0/8\t2001:db8::/32").unwrap();
        assert_eq!(list.len(), 3);
        assert!(allows(&list, ip("10.1.2.3")));
        assert!(allows(&list, ip("127.0.0.1")));
        assert!(allows(&list, ip("2001:db8::5")));
        assert!(!allows(&list, ip("8.8.8.8")));
    }

    #[test]
    fn malformed_entries_are_rejected_with_a_reason() {
        for bad in [
            "999.1.1.1",
            "10.0.0.0/33",
            "2001:db8::/129",
            "hello",
            "10.0.0.0/x",
        ] {
            assert!(parse_list(bad).is_err(), "{bad:?} must not parse");
        }
        // A whole list fails if any entry is bad, so a typo can't silently
        // widen access.
        assert!(parse_list("127.0.0.1, nonsense").is_err());
    }

    #[test]
    fn a_zero_prefix_is_explicit_any() {
        let list = parse_list("0.0.0.0/0").unwrap();
        assert!(allows(&list, ip("8.8.8.8")));
        assert!(!allows(&list, ip("::1")), "an IPv4 rule stays IPv4");
    }
}
