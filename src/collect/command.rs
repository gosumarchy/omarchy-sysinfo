//! Running a helper program without trusting it to finish.
//!
//! `Command::output` waits forever. A `df` stuck on a dead NFS mount, or an
//! `iw` waiting on a wedged driver, would hold the whole report hostage, so
//! every program this crate starts goes through [`run`] and gets a deadline.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// How often a finished-but-unreaped child is polled.
const POLL: Duration = Duration::from_millis(5);

/// Run `program` and return its trimmed stdout, or `None` if it could not be
/// started, failed, printed nothing, or overran `timeout` (it is then killed).
///
/// Stdin is closed so the child cannot steal keystrokes from the TUI, and
/// stderr is discarded so its complaints cannot scribble over the screen.
pub(crate) fn run(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;

    // The pipe is drained on its own thread: a child that fills the pipe
    // buffer blocks until someone reads, so polling `try_wait` alone could
    // deadlock on a chatty program.
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        let _ = tx.send(bytes);
    });

    let Ok(bytes) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) else {
        kill(&mut child);

        return None;
    };

    if !wait_until(&mut child, deadline)? {
        return None;
    }

    let text = String::from_utf8_lossy(&bytes).trim().to_string();

    (!text.is_empty()).then_some(text)
}

/// Reap the child, killing it if it is still alive at the deadline.
///
/// `Some(true)` means it exited successfully, `Some(false)` that it failed,
/// and `None` that it had to be killed.
fn wait_until(child: &mut Child, deadline: Instant) -> Option<bool> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status.success()),
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL),
            Ok(None) | Err(_) => {
                kill(child);

                return None;
            }
        }
    }
}

fn kill(child: &mut Child) {
    let _ = child.kill();
    // Reap it so it does not linger as a zombie.
    let _ = child.wait();
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    const SECOND: Duration = Duration::from_secs(1);

    #[test]
    fn stdout_of_a_successful_program_is_returned_trimmed() {
        assert_eq!(
            run("sh", &["-c", "echo '  hello  '"], SECOND).as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn a_failing_program_is_none_even_with_output() {
        assert_eq!(run("sh", &["-c", "echo partial; exit 3"], SECOND), None);
    }

    #[test]
    fn a_missing_program_is_none() {
        assert_eq!(run("definitely-not-a-program-xyz", &[], SECOND), None);
    }

    #[test]
    fn empty_output_is_none() {
        assert_eq!(run("true", &[], SECOND), None);
    }

    #[test]
    fn a_program_that_overruns_is_killed_promptly() {
        let started = Instant::now();

        assert_eq!(run("sleep", &["5"], Duration::from_millis(100)), None);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_program_that_closes_stdout_but_keeps_running_is_killed() {
        let started = Instant::now();

        assert_eq!(
            run(
                "sh",
                &["-c", "echo early; exec 1>&-; sleep 5"],
                Duration::from_millis(200)
            ),
            None
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn large_output_does_not_deadlock_on_the_pipe_buffer() {
        // Far more than the 64 KiB a pipe holds.
        let out = run(
            "sh",
            &["-c", "yes x | head -n 100000"],
            Duration::from_secs(5),
        )
        .expect("output");

        assert_eq!(out.lines().count(), 100_000);
    }

    #[test]
    fn the_child_cannot_read_our_stdin() {
        // With stdin inherited, `cat` would block on the terminal (or steal
        // the TUI's keystrokes). Closed, it sees EOF at once.
        assert_eq!(run("cat", &[], SECOND), None);
    }
}
