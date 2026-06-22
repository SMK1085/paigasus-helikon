//! Shared networking policy: host allow/deny matching, SSRF IP classifier,
//! and the guarded DNS resolver. Promoted from `web::http` in SMA-437 so the
//! `microvm` egress proxy can share the same primitives without depending on
//! the `web` feature.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use paigasus_helikon_core::ToolError;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};

/// Build a `reqwest::Client` with a fixed user-agent and timeout. When
/// `follow_redirects` is false the client never auto-redirects (WebFetch drives
/// redirects itself so it can re-run the SSRF check on every hop). When
/// `dns_guard` is `Some(allow_private)`, a [`GuardedResolver`] is installed so
/// connect-time DNS results are validated through [`ip_blocked`] — pinning the
/// connection to vetted IPs and closing the DNS-rebinding TOCTOU. `None` uses
/// the default resolver (search backends, which hit fixed public API hosts).
pub(crate) fn build_client(
    user_agent: &str,
    timeout: Duration,
    follow_redirects: bool,
    dns_guard: Option<bool>,
) -> reqwest::Result<reqwest::Client> {
    let redirect = if follow_redirects {
        reqwest::redirect::Policy::default()
    } else {
        reqwest::redirect::Policy::none()
    };
    let mut builder = reqwest::Client::builder()
        .user_agent(user_agent.to_owned())
        .timeout(timeout)
        .redirect(redirect);
    if let Some(allow_private) = dns_guard {
        builder = builder.dns_resolver(Arc::new(GuardedResolver { allow_private }));
    }
    builder.build()
}

/// A `reqwest` DNS resolver that drops resolved addresses failing [`ip_blocked`]
/// (unless `allow_private`), so the connection is pinned to validated IPs. This
/// is the connect-time half of the SSRF guard: it closes the rebinding window
/// between the pre-flight `ssrf_check` and reqwest's own resolution. Numeric-IP
/// hosts never reach a resolver — they are covered by the literal-IP branch of
/// [`ssrf_check`].
pub struct GuardedResolver {
    /// When `true`, private/loopback/link-local addresses are permitted (no SSRF
    /// filtering). When `false`, all addresses classified as blocked by
    /// [`ip_blocked`] are filtered out before the connection is made.
    pub allow_private: bool,
}

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let allow_private = self.allow_private;
        Box::pin(async move {
            let host = name.as_str().to_owned();
            let resolved = tokio::net::lookup_host((host.as_str(), 0)).await?;
            let addrs: Vec<SocketAddr> = if allow_private {
                resolved.collect()
            } else {
                resolved.filter(|a| !ip_blocked(a.ip())).collect()
            };
            if addrs.is_empty() {
                let err: Box<dyn std::error::Error + Send + Sync> =
                    format!("no allowed addresses resolved for `{host}` (blocked by SSRF guard)")
                        .into();
                return Err(err);
            }
            let iter: Addrs = Box::new(addrs.into_iter());
            Ok(iter)
        })
    }
}

/// `true` if `host` is permitted by the allow/deny lists. A list entry matches
/// when `host` equals it or is a sub-domain of it (case-insensitive). A deny
/// match always refuses; with an allow-list set, only matching hosts pass.
pub(crate) fn host_allowed(host: &str, allow: Option<&[String]>, deny: &[String]) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let matches = |entry: &String| {
        let e = entry.trim_end_matches('.').to_ascii_lowercase();
        host == e || host.ends_with(&format!(".{e}"))
    };
    if deny.iter().any(matches) {
        return false;
    }
    match allow {
        Some(list) => list.iter().any(matches),
        None => true,
    }
}

/// `true` if `ip` is in a range the SSRF guard refuses: loopback, RFC1918
/// private, link-local (incl. `169.254.169.254`), CGNAT, IPv6 ULA, or
/// unspecified. v4-mapped v6 addresses are unwrapped and re-checked as v4.
pub fn ip_blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4_blocked(v4),
        IpAddr::V6(v6) => {
            // v6-specific ranges first, so `::1` / `::` are caught before the
            // v4 unwrap (`to_ipv4()` would map `::1` to the non-blocked
            // `0.0.0.1`).
            if v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || is_ula(v6)
                || is_v6_link_local(v6)
                || is_v6_documentation(v6)
            {
                return true;
            }
            // Unwrap both `::ffff:a.b.c.d` (mapped) and the deprecated
            // `::a.b.c.d` (compatible) and classify the embedded v4.
            if let Some(v4) = v6.to_ipv4_mapped().or_else(|| v6.to_ipv4()) {
                return v4_blocked(v4);
            }
            false
        }
    }
}

fn v4_blocked(ip: Ipv4Addr) -> bool {
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || is_cgnat(ip)
        || is_benchmarking_v4(ip)
        || is_reserved_future_v4(ip)
}

/// `100.64.0.0/10` (RFC 6598 carrier-grade NAT). `std`'s predicate is unstable.
fn is_cgnat(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 100 && (64..=127).contains(&o[1])
}

/// `198.18.0.0/15` (RFC 2544 benchmarking). `std`'s predicate is unstable.
fn is_benchmarking_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 198 && (18..=19).contains(&o[1])
}

/// `240.0.0.0/4` (RFC 1112 reserved-for-future-use; `255.255.255.255` is also
/// caught by `is_broadcast`). `std`'s predicate is unstable.
fn is_reserved_future_v4(ip: Ipv4Addr) -> bool {
    ip.octets()[0] >= 240
}

/// `2001:db8::/32` (RFC 3849 documentation). `std`'s predicate is unstable.
fn is_v6_documentation(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    s[0] == 0x2001 && s[1] == 0x0db8
}

/// `fc00::/7` (RFC 4193 unique-local). `std`'s predicate is unstable.
fn is_ula(ip: Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xfe00) == 0xfc00
}

/// `fe80::/10` (link-local). `std`'s predicate is unstable.
fn is_v6_link_local(ip: Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xffc0) == 0xfe80
}

/// SSRF guard: refuse a URL whose host is, or resolves to, a blocked IP. A
/// no-op when `allow_private` is true. Resolution failure is operational
/// (`Other`); a blocked address is a deliberate refusal (`Denied`).
pub(crate) async fn ssrf_check(url: &url::Url, allow_private: bool) -> Result<(), ToolError> {
    if allow_private {
        return Ok(());
    }
    let denied = |host: &str| ToolError::Denied {
        reason: format!(
            "host `{host}` resolves to a blocked (private/loopback/link-local) address"
        ),
    };
    match url.host() {
        Some(url::Host::Ipv4(ip)) => {
            if ip_blocked(IpAddr::V4(ip)) {
                return Err(denied(&ip.to_string()));
            }
        }
        Some(url::Host::Ipv6(ip)) => {
            if ip_blocked(IpAddr::V6(ip)) {
                return Err(denied(&ip.to_string()));
            }
        }
        Some(url::Host::Domain(host)) => {
            let port = url.port_or_known_default().unwrap_or(80);
            let addrs = tokio::net::lookup_host((host, port)).await.map_err(|e| {
                ToolError::Other(anyhow::anyhow!("DNS resolution failed for `{host}`: {e}"))
            })?;
            let mut any = false;
            for addr in addrs {
                any = true;
                if ip_blocked(addr.ip()) {
                    return Err(denied(host));
                }
            }
            if !any {
                return Err(ToolError::Other(anyhow::anyhow!(
                    "no addresses resolved for `{host}`"
                )));
            }
        }
        None => {
            return Err(ToolError::Denied {
                reason: "URL has no host".to_owned(),
            });
        }
    }
    Ok(())
}

/// Domain allow/deny + private-IP (SSRF) policy shared by the `web` tools and the
/// `microvm` egress proxy/backend. The single public policy type (SMA-437).
///
/// Domain matching is sub-domain-aware, case-insensitive, and trailing-dot-
/// insensitive: `example.com` matches `example.com` and `api.example.com`.
///
/// **Empty-allow-list semantics matter:** `allow: None` means *no restriction*
/// (any host, subject to `deny`); `allow: Some(empty)` means *deny everything*.
/// `deny_all()` builds the latter; `allow_all()` the former.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EgressPolicy {
    allow: Option<Vec<String>>,
    deny: Vec<String>,
    allow_private_ips: bool,
}

impl EgressPolicy {
    /// Deny all egress (an empty allow-list permits nothing).
    pub fn deny_all() -> Self {
        Self {
            allow: Some(Vec::new()),
            deny: Vec::new(),
            allow_private_ips: false,
        }
    }

    /// Allow all egress (no allow-list, no deny-list).
    pub fn allow_all() -> Self {
        Self::default()
    }

    /// Add allowed domains. Setting any allow-list switches the policy to
    /// default-deny (only listed domains and their sub-domains are permitted).
    pub fn allow_domains<I, S>(mut self, domains: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allow
            .get_or_insert_with(Vec::new)
            .extend(domains.into_iter().map(Into::into));
        self
    }

    /// Add denied domains. A deny match always refuses, beating any allow.
    pub fn deny_domains<I, S>(mut self, domains: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.deny.extend(domains.into_iter().map(Into::into));
        self
    }

    /// Permit private/loopback/link-local IPs (default: deny them as SSRF risks).
    pub fn allow_private_ips(mut self, allow: bool) -> Self {
        self.allow_private_ips = allow;
        self
    }

    /// `true` if `host` is permitted: not denied, and — when an allow-list is set
    /// — matching it (itself or a sub-domain).
    pub fn is_host_allowed(&self, host: &str) -> bool {
        host_allowed(host, self.allow.as_deref(), &self.deny)
    }

    /// `true` if `ip` may be connected to: a public address, or any address when
    /// `allow_private_ips` is set.
    pub fn is_ip_allowed(&self, ip: std::net::IpAddr) -> bool {
        self.allow_private_ips || !ip_blocked(ip)
    }

    /// Deprecated alias for [`Self::is_host_allowed`], kept for source
    /// compatibility with the SMA-416 `EgressPolicy`.
    #[deprecated(note = "renamed to is_host_allowed")]
    pub fn is_allowed(&self, host: &str) -> bool {
        self.is_host_allowed(host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn ip_blocked_rejects_private_and_special_ranges() {
        for s in [
            "127.0.0.1",       // loopback
            "10.0.0.1",        // RFC1918
            "172.16.0.1",      // RFC1918
            "192.168.1.1",     // RFC1918
            "169.254.169.254", // link-local / cloud metadata
            "100.64.0.1",      // CGNAT
            "0.0.0.0",         // unspecified
            "198.18.0.1",      // benchmarking 198.18.0.0/15
            "198.19.0.1",      // benchmarking 198.18.0.0/15
            "240.0.0.1",       // reserved-for-future 240.0.0.0/4
            "::1",             // v6 loopback
            "fc00::1",         // v6 ULA
            "fe80::1",         // v6 link-local
            "ff02::1",         // v6 multicast
            "2001:db8::1",     // v6 documentation 2001:db8::/32
        ] {
            assert!(ip_blocked(ip(s)), "{s} should be blocked");
        }
    }

    #[test]
    fn ip_blocked_allows_public_addresses() {
        for s in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(!ip_blocked(ip(s)), "{s} should be allowed");
        }
    }

    #[test]
    fn ip_blocked_unwraps_v4_mapped_v6() {
        let mapped = IpAddr::V6(Ipv4Addr::new(169, 254, 169, 254).to_ipv6_mapped());
        assert!(ip_blocked(mapped));
    }

    #[test]
    fn ip_blocked_handles_v6_loopback_and_compat_forms() {
        // ::1 / :: must stay blocked even though to_ipv4() maps ::1 -> 0.0.0.1
        assert!(ip_blocked(ip("::1")));
        assert!(ip_blocked(ip("::")));
        // deprecated v4-compatible ::a.b.c.d embedding a blocked v4
        assert!(ip_blocked(ip("::169.254.169.254")));
        assert!(ip_blocked(ip("::10.0.0.1")));
        // v4-mapped public address stays allowed
        assert!(!ip_blocked(ip("::ffff:8.8.8.8")));
    }

    #[test]
    fn host_allowed_ignores_trailing_dot() {
        let deny = vec!["evil.test".to_string()];
        assert!(!host_allowed("evil.test.", None, &deny));
        assert!(!host_allowed("api.evil.test.", None, &deny));
    }

    #[test]
    fn host_allowed_deny_beats_allow_and_matches_subdomains() {
        let deny = vec!["evil.test".to_string()];
        // deny wins
        assert!(!host_allowed("evil.test", None, &deny));
        assert!(!host_allowed("api.evil.test", None, &deny)); // subdomain
                                                              // unrelated host with no allow-list passes
        assert!(host_allowed("good.test", None, &deny));
        // allow-list restricts
        let allow = Some(vec!["docs.rs".to_string()]);
        assert!(host_allowed("docs.rs", allow.as_deref(), &[]));
        assert!(host_allowed("api.docs.rs", allow.as_deref(), &[])); // subdomain
        assert!(!host_allowed("crates.io", allow.as_deref(), &[]));
        // case-insensitive
        assert!(!host_allowed("EVIL.test", None, &deny));
        // partial-label is NOT a match
        assert!(host_allowed("notevil.test", None, &deny));
    }

    #[tokio::test]
    async fn guarded_resolver_filters_loopback_unless_allowed() {
        use reqwest::dns::{Name, Resolve};
        use std::str::FromStr;

        // `localhost` resolves to loopback (127.0.0.1 / ::1) — all blocked, so
        // the guard leaves no addresses and the resolution fails.
        let blocked = GuardedResolver {
            allow_private: false,
        };
        assert!(
            blocked
                .resolve(Name::from_str("localhost").unwrap())
                .await
                .is_err(),
            "loopback must be filtered out, leaving no addresses"
        );

        // Passthrough when private IPs are explicitly allowed.
        let allowed = GuardedResolver {
            allow_private: true,
        };
        let addrs = allowed
            .resolve(Name::from_str("localhost").unwrap())
            .await
            .expect("passthrough resolves localhost");
        assert!(addrs.count() >= 1, "passthrough returns loopback addresses");
    }

    #[test]
    fn egress_policy_host_and_ip_checks() {
        use std::net::IpAddr;
        let p = EgressPolicy::deny_all().allow_domains(["pypi.org"]);
        assert!(p.is_host_allowed("pypi.org"));
        assert!(p.is_host_allowed("files.pypi.org"));
        assert!(!p.is_host_allowed("evil.test"));
        // private IPs blocked by default; allowed when toggled
        let priv_ip: IpAddr = "10.0.0.1".parse().unwrap();
        assert!(!p.is_ip_allowed(priv_ip));
        let pub_ip: IpAddr = "8.8.8.8".parse().unwrap();
        assert!(p.is_ip_allowed(pub_ip));
        let p2 = EgressPolicy::allow_all().allow_private_ips(true);
        assert!(p2.is_ip_allowed(priv_ip));
    }

    #[test]
    fn egress_policy_deprecated_is_allowed_alias_still_works() {
        let p = EgressPolicy::allow_all().deny_domains(["evil.test"]);
        #[allow(deprecated)]
        let denied = p.is_allowed("evil.test");
        assert!(!denied);
    }

    #[test]
    fn egress_policy_empty_allow_list_means_deny_all_for_forkd_default() {
        let p = EgressPolicy::deny_all(); // allow: Some(empty) -> deny everything
        assert!(!p.is_host_allowed("anything.test"));
    }
}
