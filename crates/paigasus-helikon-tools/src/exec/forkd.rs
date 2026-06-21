//! [`ForkdBackend`] — the microVM execution tier: a portable REST client of the
//! forkd Firecracker controller. Feature-gated behind `microvm`. **Experimental
//! skeleton** (SMA-416): the fork→exec→destroy flow is real but the live KVM run
//! and egress *enforcement* are deferred to SMA-437; `guarantees().network` is
//! honestly `None`.

/// Construction-time failures for [`ForkdBackend`]. Runtime failures (daemon
/// unreachable, fork/exec error) surface as `ToolError::Other` from `run`.
///
/// Variants never embed the bearer token — keep auth material out of error text.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ForkdError {
    /// The controller URL could not be parsed.
    #[error("invalid forkd controller URL: {0}")]
    InvalidUrl(String),
    /// A required field (bearer token / snapshot tag) was not set.
    #[error("missing required forkd config: {0}")]
    MissingConfig(&'static str),
    /// The controller CA PEM could not be parsed.
    #[error("invalid controller CA certificate")]
    InvalidCa,
    /// The reqwest client could not be constructed.
    #[error("failed to build forkd HTTP client")]
    ClientBuild,
}

/// `POST /v1/sandboxes` request body — fork `n` children (we use 1) copy-on-write
/// from a warmed snapshot, each in its own network namespace.
#[derive(serde::Serialize)]
struct ForkReq<'a> {
    snapshot_tag: &'a str,
    n: u32,
    per_child_netns: bool,
}

/// One sandbox in the `POST /v1/sandboxes` response **array**. forkd returns more
/// fields (snapshot_tag, guest_addr, …); only `id` is needed, the rest are ignored.
#[derive(serde::Deserialize)]
struct SandboxInfo {
    id: String,
}

/// `POST /v1/sandboxes/{id}/exec` request body. `args` runs verbatim in the guest
/// (no shell expansion), so a shell command is wrapped as `["sh","-c","<cmd>"]`.
/// `timeout_secs` is the daemon-side cap (we also enforce one client-side).
#[derive(serde::Serialize)]
struct ExecReq<'a> {
    args: [&'a str; 3],
    timeout_secs: u64,
}

/// `POST /v1/sandboxes/{id}/exec` response — captured guest output.
#[derive(serde::Deserialize)]
struct ExecResp {
    #[serde(default)]
    stdout: String,
    #[serde(default)]
    stderr: String,
    #[serde(default)]
    exit_code: Option<i32>,
}

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
    fn fork_and_exec_responses_deserialize() {
        // Fork returns an ARRAY of sandboxes (even for n:1); we take the first.
        // Unknown fields (snapshot_tag, …) are ignored.
        let v: Vec<SandboxInfo> =
            serde_json::from_str(r#"[{"id":"sb-9","snapshot_tag":"t"}]"#).unwrap();
        assert_eq!(v[0].id, "sb-9");
        // exit_code may be absent (killed by signal) -> None.
        let e: ExecResp =
            serde_json::from_str(r#"{"stdout":"hi","stderr":"","exit_code":0}"#).unwrap();
        assert_eq!(e.stdout, "hi");
        assert_eq!(e.exit_code, Some(0));
        let e2: ExecResp = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(e2.stdout, "");
        assert_eq!(e2.exit_code, None);
    }

    #[test]
    fn requests_serialize_to_forkd_shapes() {
        // Pins the wire shape and exercises the request structs.
        let f = serde_json::to_value(ForkReq {
            snapshot_tag: "t",
            n: 1,
            per_child_netns: true,
        })
        .unwrap();
        assert_eq!(f["snapshot_tag"], "t");
        assert_eq!(f["n"], 1);
        assert_eq!(f["per_child_netns"], true);
        let e = serde_json::to_value(ExecReq {
            args: ["sh", "-c", "echo hi"],
            timeout_secs: 30,
        })
        .unwrap();
        assert_eq!(e["args"][0], "sh");
        assert_eq!(e["args"][2], "echo hi");
        assert_eq!(e["timeout_secs"], 30);
    }

    #[test]
    fn forkd_error_never_embeds_a_token() {
        // Construction errors must be safe to log: no auth material in Display.
        let e = ForkdError::MissingConfig("bearer_token");
        let s = e.to_string();
        assert!(s.contains("bearer_token"));
        assert!(!s.to_lowercase().contains("secret"));
    }

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
