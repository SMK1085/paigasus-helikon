//! [`ForkdBackend`] — the microVM execution tier: a portable REST client of the
//! forkd Firecracker controller. Feature-gated behind `microvm`. **Experimental
//! skeleton** (SMA-416): the fork→exec→destroy flow is real but the live KVM run
//! and egress *enforcement* are deferred to SMA-437; `guarantees().network` is
//! honestly `None`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use paigasus_helikon_core::ToolError;

use super::{
    ExecOutput, ExecRequest, ExecutionBackend, Isolation, SandboxGuarantees, DEFAULT_MAX_OUTPUT,
    DEFAULT_TIMEOUT,
};

const DEFAULT_UA: &str = concat!("paigasus-helikon-tools/", env!("CARGO_PKG_VERSION"));
/// Fixed control-plane timeout for the destroy call (the command timeout governs exec).
const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);

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

/// Builder for [`ForkdBackend`].
pub struct ForkdBackendBuilder {
    controller_url: String,
    bearer_token: Option<String>,
    controller_ca: Option<Vec<u8>>,
    snapshot: Option<String>,
    timeout: Duration,
    max_output_bytes: usize,
    egress: EgressPolicy,
}

impl ForkdBackendBuilder {
    /// Bearer token presented to the controller (required).
    pub fn bearer_token(mut self, token: impl Into<String>) -> Self {
        self.bearer_token = Some(token.into());
        self
    }

    /// PEM trust root / cert pin for the controller's TLS (required for a
    /// self-signed localhost daemon; use a real CA for a remote host).
    pub fn controller_ca(mut self, pem: impl Into<Vec<u8>>) -> Self {
        self.controller_ca = Some(pem.into());
        self
    }

    /// Warmed parent snapshot tag to fork children from (required; forkd's
    /// `snapshot_tag`).
    pub fn snapshot(mut self, tag: impl Into<String>) -> Self {
        self.snapshot = Some(tag.into());
        self
    }

    /// Wall-clock timeout for the exec step (default 30s).
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Truncate captured stdout/stderr to this many bytes each (default 1 MiB).
    pub fn max_output_bytes(mut self, n: usize) -> Self {
        self.max_output_bytes = n;
        self
    }

    /// Egress policy the backend carries (enforcement is SMA-437).
    pub fn egress_policy(mut self, policy: EgressPolicy) -> Self {
        self.egress = policy;
        self
    }

    /// Finish building into a [`ForkdBackend`] directly (useful for unit tests
    /// that need to inspect the struct fields).
    pub fn into_backend(self) -> Result<ForkdBackend, ForkdError> {
        // Validate the controller URL up front (parsed value is discarded).
        reqwest::Url::parse(&self.controller_url)
            .map_err(|_| ForkdError::InvalidUrl(self.controller_url.clone()))?;
        let token = self
            .bearer_token
            .ok_or(ForkdError::MissingConfig("bearer_token"))?;
        let snapshot = self.snapshot.ok_or(ForkdError::MissingConfig("snapshot"))?;
        let mut cb = reqwest::Client::builder()
            .user_agent(DEFAULT_UA)
            .connect_timeout(CONTROL_TIMEOUT);
        if let Some(pem) = &self.controller_ca {
            let cert = reqwest::Certificate::from_pem(pem).map_err(|_| ForkdError::InvalidCa)?;
            cb = cb.add_root_certificate(cert);
        }
        let client = cb.build().map_err(|_| ForkdError::ClientBuild)?;
        Ok(ForkdBackend {
            client,
            base: self.controller_url.trim_end_matches('/').to_string(),
            token,
            snapshot,
            timeout: self.timeout,
            max_output_bytes: self.max_output_bytes,
            egress: self.egress,
        })
    }

    /// Finish building into a shareable `Arc<dyn ExecutionBackend>`.
    pub fn build(self) -> Result<Arc<dyn ExecutionBackend>, ForkdError> {
        Ok(Arc::new(self.into_backend()?))
    }
}

/// The microVM execution backend — a REST client of the forkd controller. See
/// the module docs: experimental skeleton; egress is carried but not enforced.
#[derive(Debug)]
pub struct ForkdBackend {
    client: reqwest::Client,
    base: String,
    token: String,
    snapshot: String,
    timeout: Duration,
    max_output_bytes: usize,
    /// Egress policy carried by the backend (enforcement is SMA-437).
    pub egress: EgressPolicy,
}

impl ForkdBackend {
    /// Start building a backend against the controller at `controller_url`
    /// (e.g. `"https://127.0.0.1:8889"`). Defaults: 30s timeout, 1 MiB output
    /// cap, `EgressPolicy::deny_all()`.
    pub fn builder(controller_url: impl Into<String>) -> ForkdBackendBuilder {
        ForkdBackendBuilder {
            controller_url: controller_url.into(),
            bearer_token: None,
            controller_ca: None,
            snapshot: None,
            timeout: DEFAULT_TIMEOUT,
            max_output_bytes: DEFAULT_MAX_OUTPUT,
            egress: EgressPolicy::deny_all(),
        }
    }

    async fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        body: &impl serde::Serialize,
    ) -> Result<T, ToolError> {
        // The bearer token rides only in the Authorization header — never in
        // the URL/body — so reqwest's error Display (URL only) cannot leak it.
        let resp = self
            .client
            .post(url)
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await
            .map_err(|e| ToolError::Other(anyhow::anyhow!("forkd request failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(ToolError::Other(anyhow::anyhow!(
                "forkd controller returned HTTP {}",
                resp.status().as_u16()
            )));
        }
        resp.json::<T>()
            .await
            .map_err(|e| ToolError::Other(anyhow::anyhow!("forkd response decode failed: {e}")))
    }

    async fn fork(&self) -> Result<String, ToolError> {
        let url = format!("{}/v1/sandboxes", self.base);
        let body = ForkReq {
            snapshot_tag: &self.snapshot,
            n: 1,
            per_child_netns: true,
        };
        // Fork returns an array (n children); we requested 1, so take the first.
        let list: Vec<SandboxInfo> =
            tokio::time::timeout(self.timeout, self.post_json(&url, &body))
                .await
                .map_err(|_| ToolError::Other(anyhow::anyhow!("forkd: fork timed out")))??;
        list.into_iter()
            .next()
            .map(|s| s.id)
            .ok_or_else(|| ToolError::Other(anyhow::anyhow!("forkd returned no sandbox")))
    }

    async fn exec(&self, id: &str, command: &str) -> Result<ExecResp, ToolError> {
        let url = format!("{}/v1/sandboxes/{id}/exec", self.base);
        // `args` runs verbatim in the guest, so wrap the shell command.
        self.post_json(
            &url,
            &ExecReq {
                args: ["sh", "-c", command],
                timeout_secs: self.timeout.as_secs(),
            },
        )
        .await
    }

    async fn destroy(&self, id: &str) {
        // Best-effort teardown; failures here are not surfaced to the model.
        let url = format!("{}/v1/sandboxes/{id}", self.base);
        let _ = self
            .client
            .delete(&url)
            .bearer_auth(&self.token)
            .send()
            .await;
    }
}

/// Truncate `s` to `cap` bytes on a char boundary; returns `(s, truncated)`.
fn truncate(mut s: String, cap: usize) -> (String, bool) {
    if s.len() <= cap {
        return (s, false);
    }
    let mut end = cap;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    (s, true)
}

#[async_trait]
impl ExecutionBackend for ForkdBackend {
    async fn run(&self, req: ExecRequest) -> Result<ExecOutput, ToolError> {
        let id = self.fork().await?;
        // The wall-clock command timeout governs exec; teardown always runs.
        let exec_result = tokio::time::timeout(self.timeout, self.exec(&id, &req.command)).await;
        let _ = tokio::time::timeout(CONTROL_TIMEOUT, self.destroy(&id)).await;
        match exec_result {
            Ok(Ok(resp)) => {
                let (stdout, t1) = truncate(resp.stdout, self.max_output_bytes);
                let (stderr, t2) = truncate(resp.stderr, self.max_output_bytes);
                Ok(ExecOutput::new(
                    stdout,
                    stderr,
                    resp.exit_code,
                    false,
                    t1 || t2,
                ))
            }
            Ok(Err(e)) => Err(e),
            Err(_) => Ok(ExecOutput::new(
                String::new(),
                String::new(),
                None,
                true,
                false,
            )),
        }
    }

    fn guarantees(&self) -> SandboxGuarantees {
        SandboxGuarantees::new(
            Isolation::Virtualized, // filesystem — separate guest kernel + rootfs
            Isolation::None,        // network — egress NOT filtered yet (SMA-437)
            Isolation::Virtualized, // syscalls — guest kernel, not a host filter
            "forkd (firecracker microvm — experimental)",
        )
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

    #[test]
    fn guarantees_are_honest() {
        let b = ForkdBackend::builder("https://localhost:8080")
            .bearer_token("t")
            .snapshot("s")
            .into_backend()
            .unwrap();
        let g = b.guarantees();
        assert_eq!(g.filesystem, Isolation::Virtualized);
        assert_eq!(g.syscalls, Isolation::Virtualized);
        assert_eq!(g.network, Isolation::None); // egress NOT enforced in the skeleton
        assert!(g.label.contains("experimental"));
    }

    #[test]
    fn builder_carries_egress_policy_and_requires_fields() {
        // Missing snapshot -> construction error.
        let err = ForkdBackend::builder("https://localhost:8080")
            .bearer_token("t")
            .into_backend()
            .unwrap_err();
        assert!(matches!(err, ForkdError::MissingConfig("snapshot")));
        // The configured policy is carried on the backend.
        let b = ForkdBackend::builder("https://localhost:8080")
            .bearer_token("t")
            .snapshot("s")
            .egress_policy(EgressPolicy::deny_all().allow_domains(["pypi.org"]))
            .into_backend()
            .unwrap();
        assert!(b.egress.is_allowed("pypi.org"));
        assert!(!b.egress.is_allowed("evil.test"));
    }
}
