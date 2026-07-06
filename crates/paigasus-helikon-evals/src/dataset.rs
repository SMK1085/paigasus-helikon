//! JSONL eval datasets.

use std::path::Path;

use crate::EvalError;

/// One evaluation case: an input plus optional expectations.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EvalCase {
    /// Case identifier (defaults to `case-<line#>` when absent in JSONL).
    #[serde(default)]
    pub id: String,
    /// The user-turn input text.
    pub input: String,
    /// Expected final output (string, or JSON for structural comparison).
    #[serde(default)]
    pub expected: Option<serde_json::Value>,
    /// Expected tool-call names, in order.
    #[serde(default)]
    pub expected_tools: Option<Vec<String>>,
    /// Free-form per-case metadata.
    #[serde(default)]
    pub metadata: serde_json::Map<String, serde_json::Value>,
}

/// A named collection of [`EvalCase`]s.
#[derive(Debug, Clone)]
pub struct EvalDataset {
    /// Dataset name (defaults to the file stem).
    pub name: String,
    /// The cases, in file order.
    pub cases: Vec<EvalCase>,
}

impl EvalDataset {
    /// Load a dataset from a JSONL file (one `EvalCase` per line; blank
    /// lines skipped).
    pub fn from_jsonl_path(path: &Path) -> Result<Self, EvalError> {
        let text = std::fs::read_to_string(path)?;
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "dataset".to_owned());
        Self::from_jsonl_str(&name, &text)
    }

    /// Parse a dataset from JSONL text.
    pub fn from_jsonl_str(name: &str, s: &str) -> Result<Self, EvalError> {
        let mut cases = Vec::new();
        for (idx, line) in s.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let mut case: EvalCase =
                serde_json::from_str(line).map_err(|source| EvalError::Parse {
                    line: idx + 1,
                    source,
                })?;
            if case.id.is_empty() {
                case.id = format!("case-{}", idx + 1);
            }
            cases.push(case);
        }
        Ok(Self {
            name: name.to_owned(),
            cases,
        })
    }
}
