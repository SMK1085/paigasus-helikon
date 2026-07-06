//! EvalDataset JSONL parsing tests.

use paigasus_helikon_evals::{EvalDataset, EvalError};

#[test]
fn parses_jsonl_with_defaults() {
    let jsonl = r#"
{"id":"greet","input":"Hi","expected":"Hello"}
{"input":"tools?","expected_tools":["lookup_spending"]}
"#;
    let ds = EvalDataset::from_jsonl_str("triage", jsonl).unwrap();
    assert_eq!(ds.name, "triage");
    assert_eq!(ds.cases.len(), 2);
    assert_eq!(ds.cases[0].id, "greet");
    assert_eq!(ds.cases[1].id, "case-3"); // 1-based line numbering, blank line 1
    assert_eq!(
        ds.cases[1].expected_tools.as_deref(),
        Some(&["lookup_spending".to_owned()][..])
    );
    assert!(ds.cases[1].expected.is_none());
}

#[test]
fn reports_parse_error_line() {
    let err = EvalDataset::from_jsonl_str("x", "{\"input\":\"ok\"}\nnot json").unwrap_err();
    assert!(matches!(err, EvalError::Parse { line: 2, .. }));
}
