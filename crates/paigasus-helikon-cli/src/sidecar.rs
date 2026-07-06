//! Parsing and validation for the `agents.toml` sidecar file format.
//!
//! A sidecar file declares one or more agents, the tools they can call, and
//! an optional evaluation configuration, all resolved by name against each
//! other. [`Sidecar::load`] (or [`Sidecar::parse`] for in-memory text) reads
//! the TOML, converts it into the public types below, and runs a validation
//! pass that catches dangling references before anything downstream (agent
//! construction, REPL, eval, MCP serving) has to deal with them.
//!
//! Paths inside the sidecar (tool scripts, instruction files, JSON schemas)
//! are stored exactly as declared — they are resolved relative to
//! [`Sidecar::base_dir`] at use time, not at parse time.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::Deserialize;

/// A fully parsed and validated `agents.toml` sidecar file.
#[derive(Debug, Clone, PartialEq)]
pub struct Sidecar {
    /// Agents declared under `[agents.*]`, keyed by agent name.
    pub agents: BTreeMap<String, AgentDef>,
    /// Tools declared under `[tools.*]`, keyed by tool name.
    pub tools: BTreeMap<String, ToolDefToml>,
    /// The optional `[eval]` section.
    pub eval: Option<EvalSection>,
    /// Directory that relative paths in the sidecar resolve against.
    pub base_dir: PathBuf,
}

/// A single agent definition from `[agents.<name>]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AgentDef {
    /// Human-readable summary of what the agent does.
    pub description: Option<String>,
    /// The agent's instructions, either inline or loaded from a file.
    pub instructions: InstructionsDef,
    /// Which model backend the agent talks to.
    pub model: ModelDef,
    /// Optional cap on the number of turns the agent may take.
    pub max_turns: Option<u32>,
    /// Names of tools (from `[tools.*]`) this agent may call.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Names of other agents (from `[agents.*]`) this agent may hand off to.
    #[serde(default)]
    pub handoffs: Vec<String>,
}

/// An agent's instructions, either given inline or loaded from a file.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum InstructionsDef {
    /// Instructions text given directly in the sidecar.
    Inline(String),
    /// Instructions loaded from an external file at use time.
    File {
        /// Path to the instructions file, resolved against `base_dir`.
        file: PathBuf,
    },
}

/// Which model backend an agent (or LLM-judge evaluator) uses.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "provider", rename_all = "lowercase")]
pub enum ModelDef {
    /// An OpenAI model, referenced by model id (e.g. `gpt-5-mini`).
    Openai {
        /// The OpenAI model id.
        id: String,
    },
    /// An Anthropic model, referenced by model id.
    Anthropic {
        /// The Anthropic model id.
        id: String,
    },
    /// A scripted mock model, useful for tests and demos.
    Mock {
        /// Path to the mock script, resolved against `base_dir`.
        script: PathBuf,
    },
}

/// A single tool definition from `[tools.<name>]`.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDefToml {
    /// Human-readable summary of what the tool does.
    pub description: String,
    /// JSON Schema describing the tool's parameters.
    pub params: serde_json::Value,
    /// Path to a script implementing the tool, resolved against `base_dir`.
    ///
    /// Exactly one of `script`/`inline` must be set.
    pub script: Option<PathBuf>,
    /// Inline script source implementing the tool.
    ///
    /// Exactly one of `script`/`inline` must be set.
    pub inline: Option<String>,
}

/// The optional `[eval]` section.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct EvalSection {
    /// Names of evaluators to run (see [`Sidecar`] validation rules for the
    /// allowed set).
    pub evaluators: Vec<String>,
    /// Config for the `json_schema` evaluator, required if it is named.
    pub json_schema: Option<JsonSchemaCfg>,
    /// Config for the `llm_judge` evaluator, required if it is named.
    pub llm_judge: Option<LlmJudgeCfg>,
}

/// Config for the `json_schema` evaluator.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct JsonSchemaCfg {
    /// Path to the JSON Schema file, resolved against `base_dir`.
    pub schema: PathBuf,
}

/// Config for the `llm_judge` evaluator.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LlmJudgeCfg {
    /// Which model the judge uses.
    pub model: ModelDef,
    /// Optional rubric text guiding the judge.
    pub rubric: Option<String>,
    /// Optional pass/fail threshold for the judge's score.
    pub threshold: Option<f64>,
}

/// Raw, pre-validation view of the sidecar as TOML deserializes it.
///
/// `tools` is kept as [`RawToolDef`] because `params` arrives as
/// [`toml::Value`] and needs an explicit conversion to
/// [`serde_json::Value`] before it becomes a public [`ToolDefToml`].
#[derive(Debug, Deserialize)]
struct RawSidecar {
    #[serde(default)]
    agents: BTreeMap<String, AgentDef>,
    #[serde(default)]
    tools: BTreeMap<String, RawToolDef>,
    eval: Option<EvalSection>,
}

/// Raw, pre-conversion view of `[tools.<name>]`.
#[derive(Debug, Deserialize)]
struct RawToolDef {
    description: String,
    params: toml::Value,
    script: Option<PathBuf>,
    inline: Option<String>,
}

/// Evaluator names recognized by the `[eval]` section.
const KNOWN_EVALUATORS: &[&str] = &["exact_match", "json_schema", "llm_judge", "tool_trajectory"];

impl Sidecar {
    /// Reads and parses the sidecar file at `path`, validating it.
    ///
    /// `base_dir` is set to `path`'s parent directory (or `.` if `path` has
    /// no parent).
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read sidecar file '{}'", path.display()))?;
        let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
        Self::parse(&text, base_dir)
    }

    /// Parses `text` as a sidecar file, validating it against `base_dir`.
    ///
    /// Paths inside the sidecar are stored as declared (not joined with
    /// `base_dir`); `base_dir` is only recorded for later use by callers
    /// that need to resolve them.
    pub fn parse(text: &str, base_dir: &Path) -> anyhow::Result<Self> {
        let raw: RawSidecar = toml::from_str(text).context("failed to parse sidecar TOML")?;

        let mut tools = BTreeMap::new();
        for (name, raw_tool) in raw.tools {
            let params = serde_json::to_value(&raw_tool.params)
                .with_context(|| format!("tool '{name}': failed to convert 'params' to JSON"))?;
            tools.insert(
                name,
                ToolDefToml {
                    description: raw_tool.description,
                    params,
                    script: raw_tool.script,
                    inline: raw_tool.inline,
                },
            );
        }

        let sidecar = Sidecar {
            agents: raw.agents,
            tools,
            eval: raw.eval,
            base_dir: base_dir.to_path_buf(),
        };

        sidecar.validate()?;
        Ok(sidecar)
    }

    /// Returns the name of the "first" agent, used as the REPL/MCP default.
    ///
    /// Agents are stored in a [`BTreeMap`], so this is the alphabetically
    /// first agent name, not necessarily the first one written in the file.
    pub fn first_agent(&self) -> Option<&str> {
        self.agents.keys().next().map(String::as_str)
    }

    /// Runs every cross-reference and shape check described on
    /// [`Sidecar`]'s module docs, bailing out on the first violation.
    fn validate(&self) -> anyhow::Result<()> {
        for (agent_name, agent) in &self.agents {
            for tool_name in &agent.tools {
                if !self.tools.contains_key(tool_name) {
                    anyhow::bail!("agent '{agent_name}' references unknown tool '{tool_name}'");
                }
            }
            for handoff in &agent.handoffs {
                if handoff == agent_name {
                    anyhow::bail!(
                        "agent '{agent_name}' declares a handoff to itself ('{handoff}')"
                    );
                }
                if !self.agents.contains_key(handoff) {
                    anyhow::bail!("agent '{agent_name}' references unknown handoff '{handoff}'");
                }
            }
        }

        validate_handoffs_acyclic(&self.agents)?;

        for (tool_name, tool) in &self.tools {
            match (&tool.script, &tool.inline) {
                (Some(_), Some(_)) => anyhow::bail!(
                    "tool '{tool_name}' declares both 'script' and 'inline' — exactly one is required"
                ),
                (None, None) => anyhow::bail!(
                    "tool '{tool_name}' declares neither 'script' nor 'inline' — exactly one is required"
                ),
                _ => {}
            }
            if !tool.params.is_object() {
                anyhow::bail!("tool '{tool_name}': 'params' must be a JSON object");
            }
        }

        if let Some(eval) = &self.eval {
            for evaluator in &eval.evaluators {
                if !KNOWN_EVALUATORS.contains(&evaluator.as_str()) {
                    anyhow::bail!("unknown evaluator '{evaluator}'");
                }
                if evaluator == "json_schema" && eval.json_schema.is_none() {
                    anyhow::bail!(
                        "evaluator 'json_schema' is declared but '[eval.json_schema]' is missing"
                    );
                }
                if evaluator == "llm_judge" && eval.llm_judge.is_none() {
                    anyhow::bail!(
                        "evaluator 'llm_judge' is declared but '[eval.llm_judge]' is missing"
                    );
                }
            }
        }

        Ok(())
    }
}

/// Depth-first search over agents' `handoffs` edges, bailing out on the
/// first cycle found.
fn validate_handoffs_acyclic(agents: &BTreeMap<String, AgentDef>) -> anyhow::Result<()> {
    let mut visited: HashSet<&str> = HashSet::new();

    for start in agents.keys() {
        if visited.contains(start.as_str()) {
            continue;
        }
        let mut stack: Vec<&str> = Vec::new();
        visit_handoffs(start, agents, &mut visited, &mut stack)?;
    }
    Ok(())
}

fn visit_handoffs<'a>(
    name: &'a str,
    agents: &'a BTreeMap<String, AgentDef>,
    visited: &mut HashSet<&'a str>,
    stack: &mut Vec<&'a str>,
) -> anyhow::Result<()> {
    if stack.contains(&name) {
        anyhow::bail!("handoff cycle detected involving '{name}' — declare one-way handoff chains");
    }
    if visited.contains(name) {
        return Ok(());
    }

    stack.push(name);
    if let Some(agent) = agents.get(name) {
        for next in &agent.handoffs {
            visit_handoffs(next.as_str(), agents, visited, stack)?;
        }
    }
    stack.pop();
    visited.insert(name);

    Ok(())
}
