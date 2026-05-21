//! Locks AC #2 — `RunResult<MyStruct>` compiles when
//! `MyStruct: DeserializeOwned + JsonSchema`, and can be produced by
//! parsing a `RunResult<String>` via
//! [`RunResult::<String>::parse_final`].

use paigasus_helikon_core::RunResult;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Default, PartialEq, Deserialize, JsonSchema)]
struct Answer {
    answer: u32,
}

#[test]
fn run_result_default_t_is_string() {
    // RunResult with no type parameter must resolve to RunResult<String>.
    let mut r: RunResult = RunResult::default();
    r.final_output = "hi".into();
    assert_eq!(r.final_output, "hi");
}

#[test]
fn run_result_with_user_struct_compiles() {
    // RunResult<MyStruct> with MyStruct: DeserializeOwned + JsonSchema.
    let mut r: RunResult<Answer> = RunResult::default();
    r.final_output.answer = 42;
    assert_eq!(r.final_output.answer, 42);
}

#[test]
fn parse_final_deserializes_json_output() {
    let mut from_runner = RunResult::<String>::default();
    from_runner.final_output = r#"{"answer": 42}"#.into();
    let typed: RunResult<Answer> = from_runner.parse_final::<Answer>().unwrap();
    assert_eq!(typed.final_output, Answer { answer: 42 });
}

#[test]
fn parse_final_propagates_serde_error_on_bad_json() {
    let mut from_runner = RunResult::<String>::default();
    from_runner.final_output = "not json".into();
    let err = from_runner.parse_final::<Answer>().unwrap_err();
    // The error came from serde_json; we just verify we got one.
    assert!(err.to_string().contains("expected"));
}
