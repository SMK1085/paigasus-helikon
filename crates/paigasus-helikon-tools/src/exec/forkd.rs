//! [`ForkdBackend`] — the microVM execution tier: a portable REST client of the
//! forkd Firecracker controller. Feature-gated behind `microvm`. **Experimental
//! skeleton** (SMA-416): the fork→exec→destroy flow is real but the live KVM run
//! and egress *enforcement* are deferred to SMA-437; `guarantees().network` is
//! honestly `None`.

/// Domain allow/deny config the backend **carries**. The skeleton does not yet
/// *enforce* egress (the netns + CONNECT-proxy layers are SMA-437); this type is
/// the seam that follow-up enforces, and the future cloud sibling shares.
///
/// Matching is sub-domain-aware, case-insensitive, and trailing-dot-insensitive:
/// `example.com` matches `example.com` and `api.example.com`.
#[derive(Debug, Clone, Default)]
pub struct EgressPolicy {
    allow: Option<Vec<String>>,
    deny: Vec<String>,
}

impl EgressPolicy {
    /// Deny all egress (an empty allow-list permits nothing).
    pub fn deny_all() -> Self {
        Self {
            allow: Some(Vec::new()),
            deny: Vec::new(),
        }
    }

    /// Allow all egress (no allow-list and no deny-list).
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

    /// `true` if `host` is permitted: not denied, and — when an allow-list is set
    /// — matching it (itself or a sub-domain).
    pub fn is_allowed(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        let matches = |entry: &String| {
            let e = entry.trim_end_matches('.').to_ascii_lowercase();
            host == e || host.ends_with(&format!(".{e}"))
        };
        if self.deny.iter().any(matches) {
            return false;
        }
        match &self.allow {
            Some(list) => list.iter().any(matches),
            None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn egress_policy_deny_all_then_allowlist() {
        let p = EgressPolicy::deny_all().allow_domains(["pypi.org"]);
        assert!(p.is_allowed("pypi.org"));
        assert!(p.is_allowed("files.pypi.org")); // sub-domain
        assert!(!p.is_allowed("evil.test")); // not on the allow-list
    }

    #[test]
    fn egress_policy_deny_beats_allow_and_default_allows() {
        let p = EgressPolicy::allow_all().deny_domains(["evil.test"]);
        assert!(!p.is_allowed("evil.test"));
        assert!(!p.is_allowed("api.evil.test")); // sub-domain
        assert!(p.is_allowed("good.test")); // no allow-list -> default allow
    }
}
