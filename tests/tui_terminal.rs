use std::process::{Command, Stdio};

#[test]
fn requires_an_interactive_terminal_before_opening_the_tui() {
    let output = Command::new(env!("CARGO_BIN_EXE_mpr2mpt-tui"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("interactive terminal"));
    assert!(output.stdout.is_empty());
}

#[test]
fn displays_help_without_an_interactive_terminal() {
    let output = Command::new(env!("CARGO_BIN_EXE_mpr2mpt-tui"))
        .arg("--help")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success());
    let usage = String::from_utf8_lossy(&output.stdout);
    assert!(
        usage.contains("[INPUT]") && usage.contains("--output"),
        "{usage}"
    );
}

#[test]
fn reports_the_tui_executable_name_in_version_output() {
    let output = Command::new(env!("CARGO_BIN_EXE_mpr2mpt-tui"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stdout.starts_with(b"mpr2mpt-tui "));
}
