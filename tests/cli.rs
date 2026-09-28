//! The command-line contract, exercised against the real binary: exit
//! statuses, where output goes, and that the plain report is safe to pipe.

#![expect(
    clippy::expect_used,
    reason = "a helper that cannot run the binary should fail the test that called it"
)]

use std::process::{Command, Output, Stdio};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_omarchy-sysinfo"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("the binary runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout is UTF-8")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr is UTF-8")
}

#[test]
fn help_goes_to_stdout_and_succeeds() {
    let out = run(&["--help"]);

    assert!(out.status.success());
    assert!(stdout(&out).contains("usage: omarchy-sysinfo"));
    assert!(stderr(&out).is_empty());
}

#[test]
fn version_names_the_crate_version() {
    let out = run(&["--version"]);

    assert!(out.status.success());
    assert_eq!(
        stdout(&out).trim(),
        format!("omarchy-sysinfo {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn a_bad_flag_exits_2_with_the_reason_on_stderr() {
    for args in [
        &["--nope"][..],
        &["--plain", "--nonsense"],
        &["--identifiers"],
    ] {
        let out = run(args);

        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(stdout(&out).is_empty(), "{args:?}");
        assert!(stderr(&out).contains("try --help"), "{args:?}");
    }
}

#[test]
fn the_plain_report_is_clean_text() {
    let out = run(&["--plain"]);

    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("OVERVIEW"), "{text}");
    assert!(
        !text.chars().any(|c| c.is_control() && c != '\n'),
        "control character in the report"
    );
}

#[test]
fn without_a_terminal_the_tui_prints_the_report_instead() {
    // Both stdout (captured) and stdin (null) are not terminals here.
    let out = run(&[]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("OVERVIEW"));
}

#[cfg(unix)]
#[test]
fn a_closed_pipe_ends_the_report_quietly() {
    use std::io::Read;

    // `--plain | head -c 1`: the reader goes away almost at once.
    let mut child = Command::new(env!("CARGO_BIN_EXE_omarchy-sysinfo"))
        .arg("--plain")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let mut first = [0u8; 1];
    child
        .stdout
        .take()
        .expect("stdout")
        .read_exact(&mut first)
        .expect("some output");
    // Dropping the pipe closes it.
    let out = child.wait_with_output().expect("wait");

    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).is_empty(), "{}", stderr(&out));
}
