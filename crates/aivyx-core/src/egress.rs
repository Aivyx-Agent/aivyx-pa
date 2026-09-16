//! Chapter Rampart — an egress guard for the network tools.
//!
//! Ward ([`crate::sensitive_paths`]) closed the *read* half of exfiltration;
//! this closes the *send* half's sharpest, default-safe edge. Two concerns:
//!
//! 1. **SSRF / private-network reach (default-on).** A network-capable agent —
//!    especially one steered by injected web/email content — fetching
//!    `http://169.254.169.254/…` (the AWS/GCP/Azure metadata endpoint →
//!    cloud-credential theft), `http://localhost:7843/…` (local services,
//!    including Aivyx's own daemon), or an RFC-1918 LAN address is a real
//!    exfiltration / pivot vector that neither `fs_root` nor the read guard
//!    touches. Public-web research uses public hostnames, so refusing
//!    loopback / link-local / private / unique-local targets by default costs
//!    normal use nothing.
//! 2. **Host allow-list (opt-in).** `[access] allow_egress_hosts` restricts the
//!    network tools to named hosts — a hard gate (unlike the model-cooperative
//!    `confirm_destructive`) for the privacy-conscious operator.
//!
//! Applied to the initial URL AND every redirect hop of `web.fetch` /
//! `web.extract` / `web.post`, so a public URL that 3xx-redirects to
//! `169.254.169.254` is caught too.
//!
//! ## Honest scope
//!
//! This checks the URL's **host literal**. A public hostname that *resolves*
//! to a private address (DNS rebinding, or attacker-controlled DNS) is not
//! caught here — robust defense resolves and re-checks the connected IP, a
//! documented follow-on. It stops the direct-address and localhost cases,
//! which are the common metadata/local-pivot attacks.

use std::net::IpAddr;

/// The egress policy: the default-on SSRF guard plus an optional operator
/// host allow-list. Cheap to clone; the network tools hold it behind `Arc`.
#[derive(Debug, Clone)]
pub struct EgressPolicy {
    /// When true (the default), refuse loopback / link-local / private /
    /// unique-local targets. `[access] allow_private_egress = true` flips it
    /// off (for operators who genuinely want the agent to reach localhost/LAN).
    block_private: bool,
    /// When non-empty, ONLY these hosts (exact or a dot-suffix subdomain
    /// match) are reachable. Empty ⇒ any public host.
    allow_hosts: Vec<String>,
}

impl Default for EgressPolicy {
    /// The default posture: SSRF guard on, no host restriction.
    fn default() -> Self {
        EgressPolicy {
            block_private: true,
            allow_hosts: Vec::new(),
        }
    }
}

impl EgressPolicy {
    pub fn new(block_private: bool, allow_hosts: Vec<String>) -> Self {
        // Normalize allow-list to lowercase for case-insensitive host match.
        let allow_hosts = allow_hosts
            .into_iter()
            .map(|h| h.trim().to_ascii_lowercase())
            .collect();
        EgressPolicy {
            block_private,
            allow_hosts,
        }
    }

    /// A fully-permissive policy (SSRF guard off, no allow-list) — the escape
    /// hatch when the operator sets `allow_private_egress` and no host list.
    pub fn permissive() -> Self {
        EgressPolicy {
            block_private: false,
            allow_hosts: Vec::new(),
        }
    }

    /// Classify a URL. `Some(reason)` ⇒ the request must be refused. Pure.
    pub fn classify(&self, url: &str) -> Option<String> {
        let host = host_of(url)?;
        self.classify_host(&host)
    }

    /// Classify a bare host (no scheme/port) — the same block-private +
    /// allow-list checks as [`Self::classify`], for callers that already have a
    /// hostname rather than a URL (e.g. `net.dns`, whose lookup query is itself
    /// a DNS-exfil channel the allow-list closes). Pure.
    pub fn classify_host(&self, host: &str) -> Option<String> {
        let host_l = host.to_ascii_lowercase();

        if self.block_private {
            // Literal IP → range checks.
            if let Ok(ip) = host_l.parse::<IpAddr>() {
                if is_blocked_ip(&ip) {
                    return Some(format!(
                        "target {host} is a private/loopback/link-local address \
                         (blocked to prevent SSRF + cloud-metadata theft)"
                    ));
                }
            } else if is_local_hostname(&host_l) {
                return Some(format!(
                    "target {host} is a local hostname (blocked; set \
                     `[access] allow_private_egress` to permit localhost/LAN)"
                ));
            }
        }

        if !self.allow_hosts.is_empty() && !self.host_allowed(&host_l) {
            return Some(format!(
                "target {host} is not in `[access] allow_egress_hosts`"
            ));
        }
        None
    }

    /// Exact host match, or a dot-boundary subdomain of an allowed host
    /// (`api.github.com` is allowed by `github.com`, but `evilgithub.com`
    /// is not).
    fn host_allowed(&self, host_l: &str) -> bool {
        self.allow_hosts
            .iter()
            .any(|a| host_l == a || host_l.ends_with(&format!(".{a}")))
    }
}

/// Extract the bare host from an `http(s)://` URL — no scheme, no userinfo, no
/// port, IPv6 brackets stripped. `None` if there's no authority.
///
/// Delegates to `url::Url`, the same WHATWG-compliant parser `reqwest` uses
/// internally, rather than a hand-rolled splitter — a hand-rolled parser
/// disagreeing with `reqwest` on edge cases (a backslash in the authority, a
/// non-dotted-decimal IPv4 literal) is exactly the gap an attacker can use to
/// make this guard see one host while `reqwest` connects to another.
fn host_of(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    match parsed.host()? {
        url::Host::Domain(d) => Some(d.to_string()),
        url::Host::Ipv4(ip) => Some(ip.to_string()),
        url::Host::Ipv6(ip) => Some(ip.to_string()),
    }
}

/// Filter resolved socket addresses down to those safe to connect to,
/// dropping any that land on a blocked (private/loopback/link-local) IP.
/// This is the TOCTOU-safe half of the SSRF guard: a custom reqwest DNS
/// resolver (in `tools::web_fetch`) runs every hostname through this, so a
/// public name that *resolves* to `127.0.0.1` / `169.254.169.254` / an
/// RFC-1918 address (DNS rebinding) is never connected to — reqwest only ever
/// sees the vetted addresses. Pure + testable.
pub(crate) fn filter_public_addrs(
    addrs: impl Iterator<Item = std::net::SocketAddr>,
) -> Vec<std::net::SocketAddr> {
    addrs.filter(|a| !is_blocked_ip(&a.ip())).collect()
}

/// Whether an IP is one the egress guard blocks by default: loopback,
/// link-local (incl. the `169.254.169.254` cloud-metadata IP), private, or
/// IPv6 unique-local (`fc00::/7`).
pub(crate) fn is_blocked_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
        }
        IpAddr::V6(v6) => {
            // An IPv4-mapped address (::ffff:a.b.c.d) must be judged by
            // its embedded IPv4 rules, not by the IPv6 loopback/ULA/
            // link-local checks alone — ::ffff:169.254.169.254 is NOT
            // ::1 and is NOT in fc00::/7 or fe80::/10, but it IS the
            // cloud-metadata address once unwrapped.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_blocked_ip(&IpAddr::V4(v4));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                // Unique-local fc00::/7.
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // Link-local fe80::/10.
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Hostnames that name the local machine.
fn is_local_hostname(host_l: &str) -> bool {
    host_l == "localhost"
        || host_l.ends_with(".localhost")
        || host_l == "ip6-localhost"
        || host_l == "localhost.localdomain"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_metadata_and_local_targets_by_default() {
        let p = EgressPolicy::default();
        for url in [
            "http://169.254.169.254/latest/meta-data/iam/security-credentials/",
            "http://127.0.0.1:7843/api",
            "http://localhost:8080/",
            "https://LocalHost/x",
            "http://10.1.2.3/internal",
            "http://192.168.1.1/",
            "http://172.16.0.9/",
            "http://[::1]:9000/",
            "http://0.0.0.0/",
        ] {
            assert!(p.classify(url).is_some(), "should block {url}");
        }
    }

    #[test]
    fn allows_public_hosts_by_default() {
        let p = EgressPolicy::default();
        for url in [
            "https://example.com/page",
            "https://api.github.com/repos/x/y",
            "http://93.184.216.34/", // a public literal IP
        ] {
            assert!(p.classify(url).is_none(), "should allow {url}");
        }
    }

    #[test]
    fn allow_private_flag_permits_localhost() {
        let p = EgressPolicy::new(false, Vec::new());
        assert!(p.classify("http://localhost:7843/").is_none());
        assert!(p.classify("http://127.0.0.1/").is_none());
    }

    #[test]
    fn host_allowlist_restricts_to_named_hosts_and_subdomains() {
        let p = EgressPolicy::new(true, vec!["github.com".into(), "example.com".into()]);
        assert!(p.classify("https://github.com/x").is_none());
        assert!(p.classify("https://api.github.com/x").is_none()); // subdomain ok
        assert!(p.classify("https://example.com/y").is_none());
        // Not in the list → blocked.
        assert!(p.classify("https://evil.com/x").is_some());
        // Look-alike must not match by suffix trick.
        assert!(p.classify("https://evilgithub.com/x").is_some());
        // Allow-list still layered under the SSRF guard.
        assert!(p.classify("http://127.0.0.1/").is_some());
    }

    #[test]
    fn filter_public_addrs_drops_private_and_keeps_public() {
        use std::net::SocketAddr;
        let addrs: Vec<SocketAddr> = [
            "127.0.0.1:80",
            "169.254.169.254:80",
            "10.0.0.5:80",
            "93.184.216.34:80", // public
            "[::1]:80",
        ]
        .iter()
        .map(|s| s.parse().unwrap())
        .collect();
        let kept = filter_public_addrs(addrs.into_iter());
        assert_eq!(kept.len(), 1, "only the public address survives");
        assert_eq!(kept[0].ip().to_string(), "93.184.216.34");
    }

    #[test]
    fn classify_host_closes_dns_exfil_via_allowlist() {
        // net.dns: with an allow-list, a hostname the agent tries to resolve
        // must be on it — closing `<secret>.attacker.com` DNS tunneling.
        let p = EgressPolicy::new(true, vec!["github.com".into()]);
        assert!(p.classify_host("api.github.com").is_none()); // allowed subdomain
        assert!(p.classify_host("secret-data.attacker.com").is_some()); // exfil host blocked
        assert!(p.classify_host("localhost").is_some()); // private blocked too
        // No allow-list ⇒ public hosts resolve (the documented inherent residual).
        let open = EgressPolicy::default();
        assert!(open.classify_host("anything.example.com").is_none());
        assert!(open.classify_host("127.0.0.1").is_some());
    }

    #[test]
    fn host_extraction_handles_userinfo_ports_ipv6() {
        assert_eq!(
            host_of("https://user:pw@example.com:443/p").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            host_of("http://[2606:2800:220:1::]:80/").as_deref(),
            Some("2606:2800:220:1::")
        );
        assert_eq!(
            host_of("https://example.com").as_deref(),
            Some("example.com")
        );
        assert_eq!(host_of("not a url"), None);
    }

    #[test]
    fn blocks_ipv4_mapped_ipv6_metadata_and_loopback() {
        for ip_str in [
            "::ffff:169.254.169.254",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.5",
        ] {
            let ip: IpAddr = ip_str.parse().unwrap();
            assert!(is_blocked_ip(&ip), "should block IPv4-mapped {ip_str}");
        }
    }

    #[test]
    fn host_of_agrees_with_url_crate_on_backslash_authority() {
        // A backslash after the host is treated as a path separator by the
        // WHATWG/url-crate parser reqwest actually uses — host_of must see
        // the SAME host reqwest will connect to, not whatever comes after
        // an rsplit('@').
        let parsed = host_of("http://169.254.169.254\\@example.com/").unwrap();
        assert_eq!(
            parsed, "169.254.169.254",
            "must match what reqwest actually connects to"
        );
    }

    #[test]
    fn host_of_normalizes_non_dotted_decimal_ipv4() {
        // 2130706433 is 127.0.0.1 as a big-endian u32 — the url crate's
        // real IPv4 parser normalizes this; host_of must match, so
        // is_blocked_ip sees a real IP literal instead of failing to parse
        // and falling through as an unrecognized hostname.
        let parsed = host_of("http://2130706433:7843/").unwrap();
        assert_eq!(parsed, "127.0.0.1");
    }

    #[test]
    fn classify_blocks_the_two_parser_divergence_urls() {
        let p = EgressPolicy::default();
        assert!(
            p.classify("http://169.254.169.254\\@example.com/")
                .is_some()
        );
        assert!(p.classify("http://2130706433:7843/").is_some());
    }
}
