//! `GraphAgent` — a declared DAG of agents; node execution gated by
//! dependencies (SMA-333, ADR-11).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::StreamExt as _;
use tracing::Instrument as _;

use crate::workflow::{assistant_text, max_depth, workflow_run_span};
use crate::{Agent, AgentError, AgentEvent, AgentInput, ContentPart, Item, RunContext, TokenUsage};

/// Errors from [`GraphAgentBuilder::build`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GraphBuildError {
    /// The graph has no nodes.
    #[error("graph has no nodes")]
    Empty,
    /// `.name(…)` was never called.
    #[error("graph has no name")]
    MissingName,
    /// Two nodes share a name.
    #[error("duplicate graph node name: {0}")]
    DuplicateNode(String),
    /// An edge references a node that doesn't exist.
    #[error("unknown graph node in edge: {0}")]
    UnknownNode(String),
    /// The declared edges contain a cycle (node names listed).
    #[error("graph contains a cycle among nodes: {0:?}")]
    Cycle(Vec<String>),
}

/// Builder for [`GraphAgent`].
pub struct GraphAgentBuilder<Ctx> {
    name: Option<String>,
    description: String,
    nodes: Vec<(String, Arc<dyn Agent<Ctx>>)>,
    edges: Vec<(String, String)>,
}

impl<Ctx> GraphAgentBuilder<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Set the graph's agent name (required).
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the graph's description.
    pub fn description(mut self, d: impl Into<String>) -> Self {
        self.description = d.into();
        self
    }

    /// Add a node.
    pub fn node(mut self, name: impl Into<String>, agent: impl Agent<Ctx> + 'static) -> Self {
        self.nodes.push((name.into(), Arc::new(agent)));
        self
    }

    /// Add a node from a shared agent.
    pub fn shared_node(mut self, name: impl Into<String>, agent: Arc<dyn Agent<Ctx>>) -> Self {
        self.nodes.push((name.into(), agent));
        self
    }

    /// Declare a dependency edge `from → to` (`to` runs after `from`).
    pub fn edge(mut self, from: impl Into<String>, to: impl Into<String>) -> Self {
        self.edges.push((from.into(), to.into()));
        self
    }

    /// Validate (duplicates, unknown endpoints, cycles) and build.
    pub fn build(self) -> Result<GraphAgent<Ctx>, GraphBuildError> {
        let name = self.name.ok_or(GraphBuildError::MissingName)?;
        if self.nodes.is_empty() {
            return Err(GraphBuildError::Empty);
        }
        let mut index: HashMap<String, usize> = HashMap::new();
        for (i, (n, _)) in self.nodes.iter().enumerate() {
            if index.insert(n.clone(), i).is_some() {
                return Err(GraphBuildError::DuplicateNode(n.clone()));
            }
        }
        let n = self.nodes.len();
        let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut succs: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut seen_edges = HashSet::new();
        for (from, to) in &self.edges {
            let f = *index
                .get(from)
                .ok_or_else(|| GraphBuildError::UnknownNode(from.clone()))?;
            let t = *index
                .get(to)
                .ok_or_else(|| GraphBuildError::UnknownNode(to.clone()))?;
            if seen_edges.insert((f, t)) {
                preds[t].push(f);
                succs[f].push(t);
            }
        }
        // Kahn's algorithm: leftover nodes are on cycles.
        let mut indegree: Vec<usize> = preds.iter().map(Vec::len).collect();
        let mut queue: VecDeque<usize> = (0..n).filter(|&i| indegree[i] == 0).collect();
        let mut visited = 0usize;
        while let Some(i) = queue.pop_front() {
            visited += 1;
            for &j in &succs[i] {
                indegree[j] -= 1;
                if indegree[j] == 0 {
                    queue.push_back(j);
                }
            }
        }
        if visited != n {
            let mut cyclic: Vec<String> = indegree
                .iter()
                .enumerate()
                .filter(|(_, d)| **d > 0)
                .map(|(i, _)| self.nodes[i].0.clone())
                .collect();
            cyclic.sort();
            return Err(GraphBuildError::Cycle(cyclic));
        }
        Ok(GraphAgent {
            name,
            description: self.description,
            nodes: self.nodes,
            preds,
            succs,
        })
    }
}

/// A declared DAG of agents with dependency-gated execution.
pub struct GraphAgent<Ctx> {
    /// The name of the graph.
    name: String,
    /// The description of the graph.
    description: String,
    /// The nodes in the graph, mapping node names to their agent instances.
    nodes: Vec<(String, Arc<dyn Agent<Ctx>>)>,
    /// Predecessor adjacency lists: `preds[i]` is the list of node indices that must complete before node `i`.
    preds: Vec<Vec<usize>>,
    /// Successor adjacency lists: `succs[i]` is the list of node indices that can run after node `i`.
    succs: Vec<Vec<usize>>,
}

impl<Ctx> fmt::Debug for GraphAgent<Ctx> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GraphAgent")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("nodes", &self.nodes.len())
            .field("preds", &self.preds)
            .field("succs", &self.succs)
            .finish()
    }
}

impl<Ctx> GraphAgent<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Start building a graph.
    pub fn builder() -> GraphAgentBuilder<Ctx> {
        GraphAgentBuilder {
            name: None,
            description: String::new(),
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }
}

#[async_trait]
impl<Ctx> Agent<Ctx> for GraphAgent<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }

    /// Run the graph as a wavefront scheduler: a node starts only after all
    /// its predecessors completed; independent ready nodes run concurrently
    /// (dynamic [`futures_util::stream::SelectAll`]). Each completed node's
    /// final text is written to `ctx.state()[<node name>]` and fed to its
    /// successors as one labeled context message per predecessor. A failed
    /// node's transitive descendants are skipped (never run, no state key);
    /// independent branches still complete, and one aggregate `RunFailed`
    /// names every failed and skipped node. On success, a single synthesized
    /// `MessageOutput` carries the sink output(s) — verbatim for one sink,
    /// deterministic JSON `{sink: text}` for more than one.
    async fn run(
        &self,
        ctx: RunContext<Ctx>,
        input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        let name = self.name.clone();
        let nodes = self.nodes.clone();
        let preds = self.preds.clone();
        let succs = self.succs.clone();

        let stream = async_stream::stream! {
            let parent_failure = ctx.failure_handle();
            let span = workflow_run_span(&name, ctx.tracer());
            yield AgentEvent::RunStarted { agent: name.clone() };

            let max = max_depth(ctx.run_config());
            if ctx.agent_depth() + 1 > max {
                let err = AgentError::MaxAgentDepthExceeded { depth: ctx.agent_depth() + 1, max };
                let msg = err.to_string();
                parent_failure.set(err);
                span.record("otel.status_code", "ERROR");
                yield AgentEvent::RunFailed { error: msg };
                return;
            }

            let n = nodes.len();
            let mut indegree: Vec<usize> = preds.iter().map(Vec::len).collect();
            let mut finals: Vec<Option<String>> = vec![None; n];
            let mut skipped = vec![false; n];
            let mut failed_nodes: Vec<usize> = Vec::new();
            let mut failures: Vec<Option<crate::FailureSlot>> = vec![None; n];
            let mut total = TokenUsage::default();
            let mut running = futures_util::stream::SelectAll::new();
            let mut ready: VecDeque<usize> =
                (0..n).filter(|&i| indegree[i] == 0).collect();

            loop {
                // Launch everything currently ready (single start site).
                while let Some(i) = ready.pop_front() {
                    let child = ctx.subagent_child();
                    failures[i] = Some(child.failure_handle());
                    yield AgentEvent::AgentUpdated { agent: nodes[i].1.name().to_owned() };

                    // Node input = original input + predecessor outputs as
                    // labeled context messages (declared-edge order).
                    let mut messages = input.messages.clone();
                    for &p in &preds[i] {
                        if let Some(text) = &finals[p] {
                            messages.push(Item::UserMessage {
                                content: vec![ContentPart::Text {
                                    text: format!("[{} output]\n{}", nodes[p].0, text),
                                }],
                            });
                        }
                    }
                    let node_input = AgentInput { messages };

                    match nodes[i].1.run(child, node_input).instrument(span.clone()).await {
                        Ok(s) => running.push(Box::pin(s.map(move |ev| (i, ev)))
                            as BoxStream<'static, (usize, AgentEvent)>),
                        Err(e) => {
                            failed_nodes.push(i);
                            // Record into the node's own slot so the aggregate
                            // pass below surfaces this typed error (a direct
                            // `parent_failure.set` here would be overwritten by
                            // the aggregate's last-write-wins `set`).
                            if let Some(f) = &failures[i] {
                                f.set(e);
                            }
                            mark_skipped(i, &succs, &mut skipped);
                        }
                    }
                }

                let Some((i, ev)) = running.next().instrument(span.clone()).await else {
                    break;
                };
                match ev {
                    AgentEvent::RunStarted { .. } => {}
                    AgentEvent::MessageOutput { item } => {
                        if let Some(t) = assistant_text(&item) {
                            finals[i] = Some(t);
                        }
                        yield AgentEvent::MessageOutput { item };
                    }
                    AgentEvent::RunCompleted { usage } => {
                        total.add(usage);
                        let node_name = nodes[i].0.clone();
                        ctx.state().set(node_name, finals[i].clone().unwrap_or_default());
                        for hook in ctx.hooks().iter() {
                            let _ = hook
                                .on_event(&ctx, &crate::HookEvent::OnSubagentStop {
                                    agent: nodes[i].1.name().to_owned(),
                                })
                                .await;
                        }
                        for &j in &succs[i] {
                            indegree[j] -= 1;
                            if indegree[j] == 0 && !skipped[j] {
                                ready.push_back(j);
                            }
                        }
                    }
                    AgentEvent::RunFailed { .. } => {
                        failed_nodes.push(i);
                        for hook in ctx.hooks().iter() {
                            let _ = hook
                                .on_event(&ctx, &crate::HookEvent::OnSubagentStop {
                                    agent: nodes[i].1.name().to_owned(),
                                })
                                .await;
                        }
                        mark_skipped(i, &succs, &mut skipped);
                    }
                    other => yield other,
                }
            }

            if !failed_nodes.is_empty() {
                let first_err = failed_nodes
                    .iter()
                    .find_map(|&i| failures[i].as_ref().and_then(|f| f.take()))
                    .unwrap_or_else(|| AgentError::Other(anyhow::anyhow!("a graph node failed")));
                let mut failed_names: Vec<&str> =
                    failed_nodes.iter().map(|&i| nodes[i].0.as_str()).collect();
                failed_names.sort_unstable();
                let mut skipped_names: Vec<&str> = skipped
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| **s)
                    .map(|(i, _)| nodes[i].0.as_str())
                    .collect();
                skipped_names.sort_unstable();
                let msg = format!(
                    "graph node(s) {failed_names:?} failed ({first_err}); skipped downstream: {skipped_names:?}"
                );
                parent_failure.set(first_err);
                span.record("otel.status_code", "ERROR");
                yield AgentEvent::RunFailed { error: msg };
                return;
            }

            // Deterministic synthesized final message from the sinks.
            let mut sink_outputs: BTreeMap<String, String> = BTreeMap::new();
            for i in 0..n {
                if succs[i].is_empty() {
                    sink_outputs.insert(nodes[i].0.clone(), finals[i].clone().unwrap_or_default());
                }
            }
            let final_text = if sink_outputs.len() == 1 {
                sink_outputs.into_values().next().unwrap_or_default()
            } else {
                serde_json::to_string(&sink_outputs).unwrap_or_else(|_| "{}".to_owned())
            };
            yield AgentEvent::MessageOutput {
                item: Item::AssistantMessage {
                    content: vec![ContentPart::Text { text: final_text }],
                    agent: Some(name.clone()),
                },
            };
            span.record("gen_ai.usage.input_tokens", total.input_tokens as i64);
            span.record("gen_ai.usage.output_tokens", total.output_tokens as i64);
            yield AgentEvent::RunCompleted { usage: total };
        };

        Ok(Box::pin(stream))
    }
}

/// Mark all transitive descendants of `i` as skipped.
fn mark_skipped(i: usize, succs: &[Vec<usize>], skipped: &mut [bool]) {
    let mut stack = vec![i];
    while let Some(k) = stack.pop() {
        for &j in &succs[k] {
            if !skipped[j] {
                skipped[j] = true;
                stack.push(j);
            }
        }
    }
}
