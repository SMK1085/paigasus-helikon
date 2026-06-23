//! Re-exports of the shared networking policy (moved to `crate::net::policy`
//! in SMA-437). Kept so the `web` modules' `use crate::web::http::…` paths and
//! the SMA-412 layout stay stable.

pub(crate) use crate::net::policy::{build_client, host_allowed, ssrf_check};
