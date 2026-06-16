#![allow(missing_docs)]

use paigasus_helikon_tools::{
    ExecOutput, ExecRequest, Isolation, ResourceLimits, SandboxGuarantees,
};

#[test]
fn exec_request_new_sets_command() {
    let req = ExecRequest::new("ls -la");
    assert_eq!(req.command, "ls -la");
}

#[test]
fn resource_limits_default_is_all_none() {
    let l = ResourceLimits::default();
    assert_eq!(l.cpu_seconds, None);
    assert_eq!(l.file_size_bytes, None);
    assert_eq!(l.address_space_bytes, None);
}

#[test]
fn guarantees_struct_holds_axes_and_label() {
    let g = SandboxGuarantees::new(
        Isolation::OsKernel,
        Isolation::None,
        Isolation::OsKernel,
        "demo",
    );
    assert_eq!(g.filesystem, Isolation::OsKernel);
    assert_eq!(g.label, "demo");
    // ExecOutput is constructible and Clone.
    let o = ExecOutput::new("out".into(), String::new(), Some(0), false, false);
    assert_eq!(o.clone().stdout, "out");
}
