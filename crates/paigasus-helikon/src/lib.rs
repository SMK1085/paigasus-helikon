#![doc = include_str!("../README.md")]

/// Trait surface and concrete types shared by every Paigasus Helikon crate. Always available.
pub use paigasus_helikon_core as core;

/// Proc macros for the SDK. Enabled via the `macros` feature.
#[cfg(feature = "macros")]
pub use paigasus_helikon_macros as macros;

/// `#[tool]` attribute macro — enabled via the `macros` feature.
#[cfg(feature = "macros")]
pub use paigasus_helikon_macros::tool;

/// `tools!` function-like macro — enabled via the `macros` feature.
#[cfg(feature = "macros")]
pub use paigasus_helikon_macros::tools;

/// OpenAI provider. Enabled via the `openai` feature.
#[cfg(feature = "openai")]
pub use paigasus_helikon_providers_openai as openai;

/// OpenAI provider — [`paigasus_helikon_providers_openai`]. Enabled via the `providers-openai` feature.
#[cfg(feature = "providers-openai")]
pub use paigasus_helikon_providers_openai as providers_openai;

/// Anthropic provider. Enabled via the `anthropic` feature.
#[cfg(feature = "anthropic")]
pub use paigasus_helikon_providers_anthropic as anthropic;

/// MCP client and server integration. Enabled via the `mcp` feature.
#[cfg(feature = "mcp")]
pub use paigasus_helikon_mcp as mcp;

/// Sandboxed Read/Write/Bash/WebFetch tools. Enabled via the `tools` feature.
#[cfg(feature = "tools")]
pub use paigasus_helikon_tools as tools;

/// Evaluation harness. Enabled via the `evals` feature.
#[cfg(feature = "evals")]
pub use paigasus_helikon_evals as evals;

/// Default ephemeral Tokio runner. Enabled via the `runtime-tokio` feature.
#[cfg(feature = "runtime-tokio")]
pub use paigasus_helikon_runtime_tokio as runtime_tokio;

/// Self-hosted Axum runtime. Enabled via the `runtime-axum` feature.
#[cfg(feature = "runtime-axum")]
pub use paigasus_helikon_runtime_axum as runtime_axum;

/// Temporal-backed durable runtime. Enabled via the `runtime-temporal` feature.
#[cfg(feature = "runtime-temporal")]
pub use paigasus_helikon_runtime_temporal as runtime_temporal;

/// AWS Bedrock AgentCore runtime. Enabled via the `runtime-agentcore` feature.
#[cfg(feature = "runtime-agentcore")]
pub use paigasus_helikon_runtime_agentcore as runtime_agentcore;

/// SQLite-backed `Session` backend. Enabled via the `sessions-sqlite` feature.
#[cfg(feature = "sessions-sqlite")]
pub use paigasus_helikon_sessions_sqlite as sessions_sqlite;

/// JSON Schema helpers.
pub mod schema {
    /// OpenAI/JSON-Schema strict-mode normalizer — see
    /// [`paigasus_helikon_core::schema::strict`].
    pub use paigasus_helikon_core::schema::strict;
}
