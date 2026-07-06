//! [`AgentRegistry`] — loads an `agents.toml` sidecar, builds live
//! [`LlmAgent`]s from it, and supports hot reload on file change.
//!
//! The registry holds the parsed [`Sidecar`] behind a [`RwLock`], so
//! callers can keep building agents from a stable snapshot while a
//! background [`Debouncer`] (via [`AgentRegistry::watch`]) reparses the
//! file and swaps it in atomically on every change. A reload that fails to
//! parse leaves the previously-loaded definitions in place — the lock is
//! only ever written to with a value that already parsed successfully.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::Context as _;
use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{new_debouncer, DebounceEventResult, Debouncer};
use paigasus_helikon_core::{Handoff, LlmAgent, Tool};

use crate::model::{build_model, build_model_for_case, CliModel};
use crate::rhai_tool::RhaiTool;
use crate::sidecar::{EvalSection, InstructionsDef, Sidecar, ToolDefToml};

/// A live view over an `agents.toml` sidecar file.
///
/// Construct via [`AgentRegistry::load`]; call [`AgentRegistry::reload`] to
/// re-parse the file on demand, or [`AgentRegistry::watch`] to reload
/// automatically whenever the file (or its directory) changes.
pub struct AgentRegistry {
    path: PathBuf,
    inner: RwLock<Sidecar>,
}

impl AgentRegistry {
    /// Loads and validates the sidecar at `path`.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let sidecar = Sidecar::load(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            inner: RwLock::new(sidecar),
        })
    }

    /// Re-reads and re-parses the sidecar file from disk.
    ///
    /// On success, the new definitions atomically replace the old ones. On
    /// **any** error (missing file, bad TOML, failed validation) the
    /// previously-loaded definitions are left untouched and the error is
    /// returned.
    pub fn reload(&self) -> anyhow::Result<()> {
        let fresh = Sidecar::load(&self.path)?;
        *self.inner.write().unwrap_or_else(|e| e.into_inner()) = fresh;
        Ok(())
    }

    /// Names of every agent currently declared in the sidecar.
    pub fn agent_names(&self) -> Vec<String> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .agents
            .keys()
            .cloned()
            .collect()
    }

    /// Whether an agent named `name` is currently declared in the sidecar.
    pub fn has_agent(&self, name: &str) -> bool {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .agents
            .contains_key(name)
    }

    /// Builds a live [`LlmAgent`] for the named agent, recursively building
    /// any handoff targets first.
    pub fn build_agent(&self, name: &str) -> anyhow::Result<LlmAgent<(), CliModel>> {
        let sidecar = self.inner.read().unwrap_or_else(|e| e.into_inner());
        build_agent_recursive(&sidecar, name, None)
    }

    /// As [`AgentRegistry::build_agent`], but resolves mock-model scripts for
    /// one eval case (falling back to the file's `default` scripts).
    pub fn build_agent_for_case(
        &self,
        name: &str,
        case_id: &str,
    ) -> anyhow::Result<LlmAgent<(), CliModel>> {
        let sidecar = self.inner.read().unwrap_or_else(|e| e.into_inner());
        build_agent_recursive(&sidecar, name, Some(case_id))
    }

    /// A clone of the sidecar's optional `[eval]` section, if present.
    pub fn eval_section(&self) -> Option<EvalSection> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .eval
            .clone()
    }

    /// Directory that the sidecar's relative paths (tool scripts, model
    /// scripts, JSON schemas, instruction files) resolve against.
    pub fn base_dir(&self) -> PathBuf {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .base_dir
            .clone()
    }

    /// Watches the sidecar's directory and calls `on_reload` with the
    /// outcome of [`AgentRegistry::reload`] every time a change settles
    /// (debounced by 300ms).
    ///
    /// The caller must keep the returned [`Debouncer`] alive for as long as
    /// it wants hot reload to keep working — dropping it stops the watch.
    pub fn watch(
        self: &Arc<Self>,
        on_reload: impl Fn(anyhow::Result<()>) + Send + 'static,
    ) -> anyhow::Result<Debouncer<RecommendedWatcher>> {
        let registry = Arc::clone(self);
        let mut debouncer = new_debouncer(
            Duration::from_millis(300),
            move |res: DebounceEventResult| {
                if res.is_ok() {
                    on_reload(registry.reload());
                }
            },
        )
        .context("failed to start sidecar file watcher")?;

        let base_dir = self
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        debouncer
            .watcher()
            .watch(&base_dir, RecursiveMode::Recursive)
            .with_context(|| {
                format!("failed to watch sidecar directory '{}'", base_dir.display())
            })?;

        Ok(debouncer)
    }
}

/// Resolves a tool definition's script/inline source into a live tool.
fn build_tool(base_dir: &Path, name: &str, def: &ToolDefToml) -> anyhow::Result<Arc<dyn Tool<()>>> {
    let source = if let Some(script) = &def.script {
        let path = base_dir.join(script);
        std::fs::read_to_string(&path).with_context(|| {
            format!(
                "failed to read script '{}' for tool '{name}'",
                path.display()
            )
        })?
    } else if let Some(inline) = &def.inline {
        inline.clone()
    } else {
        // Sidecar validation guarantees exactly one of script/inline is set.
        anyhow::bail!("tool '{name}' has neither 'script' nor 'inline'");
    };
    let tool = RhaiTool::new(
        name.to_owned(),
        def.description.clone(),
        def.params.clone(),
        &source,
    )?;
    Ok(Arc::new(tool) as Arc<dyn Tool<()>>)
}

/// Resolves an agent's instructions definition into the rendered text.
fn resolve_instructions(base_dir: &Path, def: &InstructionsDef) -> anyhow::Result<String> {
    match def {
        InstructionsDef::Inline(text) => Ok(text.clone()),
        InstructionsDef::File { file } => {
            let path = base_dir.join(file);
            std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read instructions file '{}'", path.display()))
        }
    }
}

/// Builds the named agent, recursively building handoff targets first.
///
/// Handoffs are guaranteed acyclic by [`Sidecar`]'s validation pass, so
/// plain recursion (no cycle bookkeeping) is safe here.
fn build_agent_recursive(
    sidecar: &Sidecar,
    name: &str,
    case_id: Option<&str>,
) -> anyhow::Result<LlmAgent<(), CliModel>> {
    let def = sidecar
        .agents
        .get(name)
        .with_context(|| format!("no such agent '{name}'"))?;

    let mut handoffs = Vec::with_capacity(def.handoffs.len());
    for target in &def.handoffs {
        let target_agent = build_agent_recursive(sidecar, target, case_id)?;
        handoffs.push(Handoff::to(target_agent));
    }

    let mut tools: Vec<Arc<dyn Tool<()>>> = Vec::with_capacity(def.tools.len());
    for tool_name in &def.tools {
        let tool_def = sidecar
            .tools
            .get(tool_name)
            .with_context(|| format!("agent '{name}' references unknown tool '{tool_name}'"))?;
        tools.push(build_tool(&sidecar.base_dir, tool_name, tool_def)?);
    }

    let instructions = resolve_instructions(&sidecar.base_dir, &def.instructions)
        .with_context(|| format!("resolving instructions for agent '{name}'"))?;
    let model = match case_id {
        Some(case_id) => build_model_for_case(&def.model, &sidecar.base_dir, case_id)?,
        None => build_model(&def.model, &sidecar.base_dir)?,
    };

    let mut builder = LlmAgent::builder::<()>()
        .name(name.to_owned())
        .model(model)
        .instructions(instructions)
        .tools(tools)
        .handoffs(handoffs);
    if let Some(description) = &def.description {
        builder = builder.description(description.clone());
    }
    if let Some(max_turns) = def.max_turns {
        builder = builder.max_turns(max_turns);
    }
    Ok(builder.build())
}
