//! Dispatch microbench (SMA-323): measure `Tool::invoke` dispatch overhead —
//! registry name-lookup + `dyn Tool` vtable call + JSON-output read.
//!
//! Dependency-free (no Criterion) on purpose: Criterion's transitive
//! `clap_lex` uses `edition2024`, which Cargo 1.75 cannot parse, which would
//! break the workspace's Rust 1.75 MSRV. A single `block_on` wraps the whole
//! measured loop so tokio runtime entry is amortized across all iterations —
//! the number reflects dispatch, not executor entry. Target: < 50 µs.
//!
//! Run: `cargo bench -p paigasus-helikon --bench tool_dispatch`

use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use serde_json::{json, Value};

use paigasus_helikon::core::{
    CancellationToken, Tool, ToolContext, ToolError, ToolOutput, TracerHandle,
};

/// A trivial tool: adds two amounts. The body is intentionally cheap so the
/// measurement is dominated by dispatch, not tool work.
struct SumTool {
    schema: Value,
}

#[async_trait]
impl Tool<()> for SumTool {
    fn name(&self) -> &str {
        "sum"
    }
    fn description(&self) -> &str {
        "Adds two amounts."
    }
    fn schema(&self) -> &Value {
        &self.schema
    }
    async fn invoke(&self, _ctx: &ToolContext<()>, args: Value) -> Result<ToolOutput, ToolError> {
        let a = args["a"].as_f64().unwrap_or(0.0);
        let b = args["b"].as_f64().unwrap_or(0.0);
        Ok(ToolOutput::new(json!({ "total": a + b })))
    }
}

/// Iteration counts: enough to average out per-call noise without making the
/// bench slow.
const WARMUP_ITERS: u32 = 10_000;
const MEASURED_ITERS: u32 = 200_000;

fn main() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("build current-thread runtime");

    // Heterogeneous registry, accessed by name — the realistic dispatch path.
    let registry: Vec<Arc<dyn Tool<()>>> = vec![Arc::new(SumTool {
        schema: json!({ "type": "object" }),
    })];
    let ctx = ToolContext::new(
        Arc::new(()),
        TracerHandle::default(),
        CancellationToken::new(),
        0,
        paigasus_helikon::core::RunConfig::default().max_agent_depth,
    );
    let args = json!({ "a": 19.99, "b": 4.50 });

    // One `block_on` wraps warmup + measurement so runtime entry is amortized,
    // not charged per iteration.
    let per_call = rt.block_on(async {
        for _ in 0..WARMUP_ITERS {
            dispatch_once(&registry, &ctx, &args).await;
        }
        let start = Instant::now();
        for _ in 0..MEASURED_ITERS {
            dispatch_once(&registry, &ctx, &args).await;
        }
        start.elapsed() / MEASURED_ITERS
    });

    println!("tool_dispatch: {per_call:?}/call ({MEASURED_ITERS} iters)");
    assert!(
        per_call.as_micros() < 50,
        "tool dispatch {per_call:?} exceeds the 50 µs target"
    );
}

/// One realistic dispatch: look the tool up by name in the registry, invoke it
/// through the `dyn Tool` vtable, and read the JSON output.
async fn dispatch_once(registry: &[Arc<dyn Tool<()>>], ctx: &ToolContext<()>, args: &Value) {
    let tool = registry
        .iter()
        .find(|t| t.name() == "sum")
        .expect("tool present");
    let out = tool.invoke(ctx, args.clone()).await.expect("invoke ok");
    black_box(out.content);
}
