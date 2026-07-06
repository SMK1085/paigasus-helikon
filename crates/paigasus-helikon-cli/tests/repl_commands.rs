//! `parse_repl_command` unit tests (SMA-333). The REPL loop itself is thin
//! I/O glue and isn't unit-tested here — see the manual sanity check noted
//! in the task report.

use paigasus_helikon_cli::repl::{parse_repl_command, ReplCommand};

#[test]
fn agents_command() {
    assert_eq!(parse_repl_command("/agents"), ReplCommand::Agents);
}

#[test]
fn switch_command_captures_name() {
    assert_eq!(
        parse_repl_command("/switch budgeting"),
        ReplCommand::Switch("budgeting".to_owned())
    );
}

#[test]
fn switch_command_with_no_name_is_empty_switch() {
    assert_eq!(
        parse_repl_command("/switch"),
        ReplCommand::Switch(String::new())
    );
}

#[test]
fn reload_command() {
    assert_eq!(parse_repl_command("/reload"), ReplCommand::Reload);
}

#[test]
fn quit_and_exit_commands() {
    assert_eq!(parse_repl_command("/quit"), ReplCommand::Quit);
    assert_eq!(parse_repl_command("/exit"), ReplCommand::Quit);
}

#[test]
fn quit_command_is_trimmed() {
    assert_eq!(parse_repl_command("  /quit  "), ReplCommand::Quit);
}

#[test]
fn unknown_slash_command() {
    assert_eq!(
        parse_repl_command("/unknown x"),
        ReplCommand::Unknown("/unknown x".to_owned())
    );
}

#[test]
fn plain_text_is_say() {
    assert_eq!(
        parse_repl_command("how much?"),
        ReplCommand::Say("how much?".to_owned())
    );
}

#[test]
fn empty_line_is_empty_say() {
    assert_eq!(parse_repl_command("   "), ReplCommand::Say(String::new()));
}
