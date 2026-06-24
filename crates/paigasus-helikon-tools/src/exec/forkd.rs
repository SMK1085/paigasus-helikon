//! [`ForkdBackend`] — the microVM execution tier: a portable REST client of the
//! forkd Firecracker controller. Feature-gated behind `microvm`. **Experimental**
//! (SMA-416/SMA-437): the fork→exec→destroy flow is real and egress enforcement
//! now exists — after deploying the per-VM netns rules that force traffic through a
//! [`EgressProxy`](crate::EgressProxy), call `.enforce_egress(proxy_endpoint)` to
//! *attest* that setup and report [`Isolation::Proxied`] from
//! `guarantees().network`; the default (no `.enforce_egress`) leaves it
//! [`Isolation::None`]. (The netns rules do the routing; the method only probes +
//! flips the reported guarantee.)
//! The live KVM run is validated via the harness/runbook
//! (`docs/runbooks/forkd-live-validation.md`).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use paigasus_helikon_core::ToolError;

use crate::net::policy::EgressPolicy;

use super::{
    ExecOutput, ExecRequest, ExecutionBackend, Isolation, SandboxGuarantees, DEFAULT_MAX_OUTPUT,
    DEFAULT_TIMEOUT,
};

const DEFAULT_UA: &str = concat!("paigasus-helikon-tools/", env!("CARGO_PKG_VERSION"));
/// Fixed control-plane timeout for the destroy call (the command timeout governs exec).
const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
/// Default minimum age a tag-matching sandbox must reach before [`ForkdBackend::reconcile`]
/// will reap it (10× the default exec timeout). MUST exceed your longest expected run.
const DEFAULT_REAP_AGE: Duration = Duration::from_secs(300);
/// Bounded concurrency for the reconcile reap fan-out (simultaneous in-flight DELETEs).
const REAP_CONCURRENCY: usize = 8;

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
    /// A plain-`http` controller URL on a non-loopback host — this would send the
    /// bearer token in cleartext over the network. Use `https` for remote hosts.
    #[error("insecure forkd controller URL: use https for non-loopback hosts")]
    InsecureControllerUrl,
    /// `enforce_egress` was set but the proxy endpoint could not be reached.
    #[error("egress proxy endpoint is unreachable")]
    ProxyUnreachable,
}

/// Outcome of a [`ForkdBackend::reconcile`] sweep.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ReconcileReport {
    /// Total sandboxes the controller LIST returned (across *all* snapshot tags) —
    /// observability into host load, independent of the reap set.
    pub scanned: usize,
    /// Ids successfully reaped (DELETE 2xx, or 404 = already gone → idempotent).
    pub reaped: Vec<String>,
    /// Ids that matched and were old enough but whose DELETE errored (non-404).
    /// Best-effort; non-fatal.
    pub failed: Vec<String>,
    /// Tag-matching entries whose `created_at_unix` was absent/unparseable, so they
    /// could not be aged and were **not** reaped. A high value with empty `reaped`
    /// signals the controller's LIST wire shape drifted.
    pub skipped_unageable: usize,
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

/// One sandbox in the `GET /v1/sandboxes` list response. Same item shape as the fork
/// response (SMA-416 spike §7) — we read only what reconcile needs and ignore the
/// rest. `created_at_unix` is parsed leniently so one odd entry can't fail the whole
/// decode: a missing field, `null`, or a non-integer value (string/float) all map to
/// `None` and are counted `skipped_unageable` (never reaped) rather than aborting the
/// sweep — the loud signal that the wire contract drifted.
#[derive(serde::Deserialize)]
struct SandboxListEntry {
    id: String,
    snapshot_tag: String,
    #[serde(default, deserialize_with = "lenient_unix_secs")]
    created_at_unix: Option<u64>,
}

/// Deserialize `created_at_unix` without ever erroring on a present-but-wrong-typed
/// value: accept an integer that fits `u64`, map anything else (`null`, string, float,
/// negative) to `None`. This keeps a single malformed LIST entry from aborting the
/// whole `Vec<SandboxListEntry>` decode (it becomes `skipped_unageable` instead).
fn lenient_unix_secs<'de, D>(de: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <Option<serde_json::Value> as serde::Deserialize>::deserialize(de)?;
    Ok(value.as_ref().and_then(serde_json::Value::as_u64))
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

/// Builder for [`ForkdBackend`].
pub struct ForkdBackendBuilder {
    controller_url: String,
    bearer_token: Option<String>,
    controller_ca: Option<Vec<u8>>,
    snapshot: Option<String>,
    timeout: Duration,
    max_output_bytes: usize,
    egress: EgressPolicy,
    enforce_egress: Option<String>,
    reap_age: Duration,
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

    /// Egress policy the backend carries. The policy is declared here; call
    /// `.enforce_egress()` to attest that it is enforced by the deployed
    /// [`EgressProxy`](crate::EgressProxy) + per-VM netns default-deny.
    pub fn egress_policy(mut self, policy: EgressPolicy) -> Self {
        self.egress = policy;
        self
    }

    /// Attest that the layered egress enforcement (per-VM netns default-deny + the
    /// [`EgressProxy`](crate::EgressProxy) at `proxy_endpoint`) is deployed, so
    /// `guarantees().network` reports [`Isolation::Proxied`]. `build()` probes the
    /// proxy for reachability and fails closed if it cannot connect — but it
    /// **cannot** verify the host's netns rules, so this is an operator attestation
    /// (the same trust model the kernel/hypervisor tiers use). Without this, the
    /// network guarantee stays [`Isolation::None`]. `proxy_endpoint` is `host:port`
    /// or a URL.
    pub fn enforce_egress(mut self, proxy_endpoint: impl Into<String>) -> Self {
        self.enforce_egress = Some(proxy_endpoint.into());
        self
    }

    /// Minimum age a tag-matching sandbox must reach before [`ForkdBackend::reconcile`]
    /// will reap it. MUST exceed your longest expected run **plus** any clock skew
    /// between this host and the controller host, or a long legitimate run could be
    /// reaped. Default: 300s (10× the default 30s exec timeout).
    pub fn reap_age(mut self, age: Duration) -> Self {
        self.reap_age = age;
        self
    }

    /// Finish building into the concrete [`ForkdBackend`]. Use this (instead of
    /// [`Self::build`]) when you need to call [`ForkdBackend::reconcile`], which the
    /// `Arc<dyn ExecutionBackend>` returned by `build()` cannot reach. Wrap the result
    /// in an `Arc` once and clone it to a `Arc<dyn ExecutionBackend>` for `BashTool`.
    pub fn build_backend(self) -> Result<ForkdBackend, ForkdError> {
        // Validate the controller URL up front. Reject plain `http` to a
        // non-loopback host: the bearer token would travel in cleartext, the
        // network-MITM threat the TLS-trust story exists to prevent. Loopback
        // `http` (forkd's documented default) stays allowed.
        let parsed = reqwest::Url::parse(&self.controller_url)
            .map_err(|_| ForkdError::InvalidUrl(self.controller_url.clone()))?;
        if parsed.scheme() == "http" && !host_is_loopback(&parsed) {
            return Err(ForkdError::InsecureControllerUrl);
        }
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
        let egress_enforced = match &self.enforce_egress {
            Some(ep) => {
                probe_proxy_reachable(ep).map_err(|_| ForkdError::ProxyUnreachable)?;
                true
            }
            None => false,
        };
        Ok(ForkdBackend {
            client,
            base: self.controller_url.trim_end_matches('/').to_string(),
            token,
            snapshot,
            timeout: self.timeout,
            max_output_bytes: self.max_output_bytes,
            egress: self.egress,
            egress_enforced,
            reap_age: self.reap_age,
        })
    }

    /// Finish building into a shareable `Arc<dyn ExecutionBackend>`.
    pub fn build(self) -> Result<Arc<dyn ExecutionBackend>, ForkdError> {
        Ok(Arc::new(self.build_backend()?))
    }
}

/// The microVM execution backend — a REST client of the forkd controller. See
/// the module docs: experimental skeleton; egress is carried but not enforced.
///
/// `Debug` is implemented manually to **redact the bearer `token`** — a derived
/// `Debug` would leak it into logs/traces.
pub struct ForkdBackend {
    client: reqwest::Client,
    base: String,
    token: String,
    snapshot: String,
    timeout: Duration,
    max_output_bytes: usize,
    egress: EgressPolicy,
    egress_enforced: bool,
    reap_age: Duration,
}

impl std::fmt::Debug for ForkdBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForkdBackend")
            .field("base", &self.base)
            .field("token", &"<redacted>")
            .field("snapshot", &self.snapshot)
            .field("timeout", &self.timeout)
            .field("max_output_bytes", &self.max_output_bytes)
            .field("egress", &self.egress)
            .field("reap_age", &self.reap_age)
            .finish_non_exhaustive()
    }
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
            enforce_egress: None,
            reap_age: DEFAULT_REAP_AGE,
        }
    }

    /// The egress policy this backend carries. This accessor reads the declared
    /// policy; enforcement is attested via `.enforce_egress()` at build time
    /// (see its doc for the trust model).
    pub fn egress_policy(&self) -> &EgressPolicy {
        &self.egress
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

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T, ToolError> {
        // Mirrors post_json: bearer in the header only; error text carries the URL,
        // never the token.
        let resp = self
            .client
            .get(url)
            .bearer_auth(&self.token)
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

    /// DELETE a sandbox, returning the outcome. A `404` is treated as success
    /// (already gone — idempotent under concurrent/repeat sweeps).
    async fn try_destroy(&self, id: &str) -> Result<(), ToolError> {
        let url = format!("{}/v1/sandboxes/{id}", self.base);
        let resp = self
            .client
            .delete(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| ToolError::Other(anyhow::anyhow!("forkd request failed: {e}")))?;
        if resp.status().is_success() || resp.status() == reqwest::StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(ToolError::Other(anyhow::anyhow!(
                "forkd controller returned HTTP {}",
                resp.status().as_u16()
            )))
        }
    }

    async fn destroy(&self, id: &str) {
        // Best-effort teardown; failures here are not surfaced to the model.
        let _ = self.try_destroy(id).await;
    }

    /// List the controller's sandboxes and reap orphans of this backend's snapshot
    /// tag that are strictly older than [`reap_age`](ForkdBackendBuilder::reap_age).
    ///
    /// Best-effort per sandbox: only a failed LIST returns `Err`; per-sandbox DELETE
    /// failures (non-404) land in [`ReconcileReport::failed`]. Deletes run with
    /// bounded concurrency, so worst-case latency on a degraded controller is about
    /// `CONTROL_TIMEOUT + ceil(N / REAP_CONCURRENCY) * CONTROL_TIMEOUT` for `N`
    /// candidates. Safe under the operator invariant `reap_age > longest run + skew`.
    pub async fn reconcile(&self) -> Result<ReconcileReport, ToolError> {
        let url = format!("{}/v1/sandboxes", self.base);
        let list: Vec<SandboxListEntry> =
            tokio::time::timeout(CONTROL_TIMEOUT, self.get_json(&url))
                .await
                .map_err(|_| ToolError::Other(anyhow::anyhow!("forkd: list timed out")))??;
        let scanned = list.len();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let reap_age_secs = self.reap_age.as_secs();

        let mut skipped_unageable = 0usize;
        let mut candidates: Vec<String> = Vec::new();
        for entry in list {
            if entry.snapshot_tag != self.snapshot {
                continue; // other tag — counts only toward `scanned`
            }
            match entry.created_at_unix {
                None => skipped_unageable += 1,
                Some(t) if now.saturating_sub(t) > reap_age_secs => candidates.push(entry.id),
                Some(_) => {} // young enough — protected
            }
        }

        let mut reaped = Vec::new();
        let mut failed = Vec::new();
        for chunk in candidates.chunks(REAP_CONCURRENCY) {
            let outcomes = futures_util::future::join_all(chunk.iter().map(|id| async move {
                let res = tokio::time::timeout(CONTROL_TIMEOUT, self.try_destroy(id)).await;
                (id.clone(), matches!(res, Ok(Ok(()))))
            }))
            .await;
            for (id, ok) in outcomes {
                if ok {
                    reaped.push(id);
                } else {
                    failed.push(id);
                }
            }
        }

        Ok(ReconcileReport {
            scanned,
            reaped,
            failed,
            skipped_unageable,
        })
    }
}

/// Best-effort reachability probe: a short TCP connect to the proxy endpoint.
/// Sync (callable from the sync `build()`), uses `std::net` with a 3-second timeout.
/// Accepts `host:port` or a URL (strips `http://`/`https://` scheme if present).
/// Iterates ALL resolved addresses and returns `Ok(())` on the first successful
/// connect, so a mix of unreachable IPv6 and reachable IPv4 addresses does not
/// cause a false failure.
fn probe_proxy_reachable(endpoint: &str) -> std::io::Result<()> {
    use std::net::ToSocketAddrs;
    // Strip a scheme and trailing slash if present.
    let hostport = endpoint
        .strip_prefix("http://")
        .or_else(|| endpoint.strip_prefix("https://"))
        .unwrap_or(endpoint)
        .trim_end_matches('/');
    let addrs: Vec<_> = hostport.to_socket_addrs()?.collect();
    if addrs.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no addresses resolved",
        ));
    }
    let timeout = std::time::Duration::from_secs(3);
    let mut last_err = std::io::Error::new(std::io::ErrorKind::NotFound, "no addresses resolved");
    for addr in addrs {
        match std::net::TcpStream::connect_timeout(&addr, timeout) {
            Ok(_) => return Ok(()),
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

/// `true` if `url`'s host is loopback (`localhost`, `127.0.0.0/8`, or `::1`), so a
/// plain-`http` controller there does not expose the bearer token to the network.
fn host_is_loopback(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    // `host_str` may keep IPv6 brackets (`[::1]`); strip them before comparing.
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host == "::1"
        || host
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback())
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
        // Accepted gap: if the controller commits a fork but we fail to read its id
        // (decode error / client timeout after commit), that sandbox is orphaned — we
        // have no id to DELETE here. It is reaped by the age-based `reconcile()` sweep
        // (SMA-447) once it ages past `reap_age`, provided the controller stamps a
        // parseable `created_at_unix` (otherwise it surfaces as `skipped_unageable`).
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
        let network = if self.egress_enforced {
            Isolation::Proxied
        } else {
            Isolation::None
        };
        SandboxGuarantees::new(
            Isolation::Virtualized, // filesystem — separate guest kernel + rootfs
            network,                // network — Proxied when enforce_egress is set
            Isolation::Virtualized, // syscalls — guest kernel, not a host filter
            "forkd (firecracker microvm — experimental)",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_list_entry_deserializes() {
        // Extra fields (guest_addr, pid, …) are ignored; created_at_unix may be absent.
        let with_ts: SandboxListEntry = serde_json::from_str(
            r#"{"id":"sb-1","snapshot_tag":"t","guest_addr":"10.0.0.2","created_at_unix":1718000000}"#,
        )
        .unwrap();
        assert_eq!(with_ts.id, "sb-1");
        assert_eq!(with_ts.snapshot_tag, "t");
        assert_eq!(with_ts.created_at_unix, Some(1718000000));
        let no_ts: SandboxListEntry =
            serde_json::from_str(r#"{"id":"sb-2","snapshot_tag":"t"}"#).unwrap();
        assert_eq!(no_ts.created_at_unix, None);
        // A present-but-malformed created_at_unix (string / float / null) maps to None
        // rather than failing the decode — one odd entry can't abort the whole sweep.
        for bad in [
            r#"{"id":"x","snapshot_tag":"t","created_at_unix":"not-a-number"}"#,
            r#"{"id":"x","snapshot_tag":"t","created_at_unix":1718000000.5}"#,
            r#"{"id":"x","snapshot_tag":"t","created_at_unix":null}"#,
        ] {
            let e: SandboxListEntry = serde_json::from_str(bad).unwrap();
            assert_eq!(e.created_at_unix, None, "malformed should be None: {bad}");
        }
    }

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
    fn guarantees_are_honest() {
        let b = ForkdBackend::builder("https://localhost:8080")
            .bearer_token("t")
            .snapshot("s")
            .build_backend()
            .unwrap();
        let g = b.guarantees();
        assert_eq!(g.filesystem, Isolation::Virtualized);
        assert_eq!(g.syscalls, Isolation::Virtualized);
        assert_eq!(g.network, Isolation::None); // egress NOT enforced without enforce_egress
        assert!(g.label.contains("experimental"));
    }

    #[test]
    fn guarantees_network_none_without_enforce_egress() {
        let b = ForkdBackend::builder("https://localhost:8080")
            .bearer_token("t")
            .snapshot("s")
            .build_backend()
            .unwrap();
        assert_eq!(b.guarantees().network, Isolation::None);
    }

    #[test]
    fn builder_carries_egress_policy_and_requires_fields() {
        // Missing snapshot -> construction error.
        let err = ForkdBackend::builder("https://localhost:8080")
            .bearer_token("t")
            .build_backend()
            .unwrap_err();
        assert!(matches!(err, ForkdError::MissingConfig("snapshot")));
        // The configured policy is carried on the backend.
        let b = ForkdBackend::builder("https://localhost:8080")
            .bearer_token("t")
            .snapshot("s")
            .egress_policy(EgressPolicy::deny_all().allow_domains(["pypi.org"]))
            .build_backend()
            .unwrap();
        assert!(b.egress_policy().is_host_allowed("pypi.org"));
        assert!(!b.egress_policy().is_host_allowed("evil.test"));
    }

    #[test]
    fn rejects_insecure_remote_http_controller() {
        // Remote plain-HTTP would leak the bearer token in cleartext — rejected.
        let err = ForkdBackend::builder("http://forkd.example.com:8889")
            .bearer_token("t")
            .snapshot("s")
            .build_backend()
            .unwrap_err();
        assert!(matches!(err, ForkdError::InsecureControllerUrl));
        // Loopback plain-HTTP (forkd's documented default) is allowed.
        assert!(ForkdBackend::builder("http://127.0.0.1:8889")
            .bearer_token("t")
            .snapshot("s")
            .build_backend()
            .is_ok());
        // HTTPS to a remote host is allowed.
        assert!(ForkdBackend::builder("https://forkd.example.com:8889")
            .bearer_token("t")
            .snapshot("s")
            .build_backend()
            .is_ok());
    }

    #[test]
    fn debug_redacts_the_bearer_token() {
        let b = ForkdBackend::builder("https://localhost:8080")
            .bearer_token("super-secret-token")
            .snapshot("s")
            .build_backend()
            .unwrap();
        let dbg = format!("{b:?}");
        assert!(
            !dbg.contains("super-secret-token"),
            "token leaked in Debug: {dbg}"
        );
        assert!(dbg.contains("redacted"));
    }

    #[test]
    fn builder_sets_reap_age_and_build_backend_is_public() {
        // Default reap_age is DEFAULT_REAP_AGE.
        let b = ForkdBackend::builder("https://localhost:8080")
            .bearer_token("t")
            .snapshot("s")
            .build_backend()
            .unwrap();
        assert_eq!(b.reap_age, DEFAULT_REAP_AGE);
        // A custom reap_age is carried onto the backend.
        let b2 = ForkdBackend::builder("https://localhost:8080")
            .bearer_token("t")
            .snapshot("s")
            .reap_age(Duration::from_secs(42))
            .build_backend()
            .unwrap();
        assert_eq!(b2.reap_age, Duration::from_secs(42));
    }
}
