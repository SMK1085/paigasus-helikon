//! `GraphAgent` — a declared DAG of agents; node execution gated by
//! dependencies (SMA-333, ADR-11).

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::Arc;

use crate::Agent;

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
#[allow(dead_code)]
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
