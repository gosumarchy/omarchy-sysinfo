//! Talking to Hyprland over its control socket instead of forking `hyprctl`.
//!
//! `hyprctl` is a thin client for this same socket: it writes the request,
//! reads the reply until the compositor closes the connection, and prints it.
//! Doing that here saves a process per question and, more importantly, lets
//! the read carry a timeout, so a stalled compositor cannot stall the report.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(1);

/// Replies are small (a few KiB for `monitors`); anything past this is not a
/// reply we understand and is not worth buffering.
const MAX_REPLY: u64 = 1 << 20;

/// Where this session's Hyprland listens, if we are inside one.
///
/// Hyprland 0.40 moved the socket from `/tmp/hypr` to `$XDG_RUNTIME_DIR/hypr`;
/// the old location is still tried for older releases.
pub(crate) fn socket_from_env() -> Option<PathBuf> {
    let signature = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);

    socket_path(runtime.as_deref(), Path::new(&signature))
}

fn socket_path(runtime: Option<&Path>, signature: &Path) -> Option<PathBuf> {
    // A signature is a single directory name; anything else is not one we
    // should be joining onto a path.
    if signature.components().count() != 1 {
        return None;
    }

    let candidates = runtime
        .map(|r| r.join("hypr"))
        .into_iter()
        .chain(std::iter::once(PathBuf::from("/tmp/hypr")))
        .map(|dir| dir.join(signature).join(".socket.sock"));

    candidates.into_iter().find(|p| p.exists())
}

/// Send one request (`monitors`, `workspaces`, `activewindow`, `version`) and
/// return the reply as `hyprctl` would print it.
pub(crate) fn request(socket: &Path, what: &str) -> Option<String> {
    let mut stream = UnixStream::connect(socket).ok()?;

    stream.set_read_timeout(Some(TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(TIMEOUT)).ok()?;
    stream.write_all(what.as_bytes()).ok()?;

    let mut reply = Vec::new();
    stream.take(MAX_REPLY).read_to_end(&mut reply).ok()?;

    let text = String::from_utf8_lossy(&reply).trim().to_string();

    // Hyprland answers an unknown request with this rather than an error.
    (!text.is_empty() && text != "unknown request").then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::fixture::Fixture;
    use std::os::unix::net::UnixListener;

    #[test]
    fn a_request_round_trips_through_a_socket() {
        let fx = Fixture::new();
        let path = fx.dir().join("hypr.sock");
        let listener = UnixListener::bind(&path).expect("bind");

        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 64];
            let n = conn.read(&mut buf).expect("read request");
            assert_eq!(&buf[..n], b"monitors");
            conn.write_all(b"Monitor eDP-1 (ID 0):\n\t1920x1080@60\n")
                .expect("reply");
        });

        let reply = request(&path, "monitors").expect("a reply");
        server.join().expect("server thread");

        assert!(reply.starts_with("Monitor eDP-1"), "{reply}");
    }

    #[test]
    fn an_unknown_request_reply_is_none() {
        let fx = Fixture::new();
        let path = fx.dir().join("hypr.sock");
        let listener = UnixListener::bind(&path).expect("bind");

        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 64];
            let _ = conn.read(&mut buf);
            conn.write_all(b"unknown request").expect("reply");
        });

        assert_eq!(request(&path, "nonsense"), None);
        server.join().expect("server thread");
    }

    #[test]
    fn a_missing_socket_is_none() {
        let fx = Fixture::new();

        assert_eq!(request(&fx.dir().join("nope.sock"), "monitors"), None);
    }

    #[test]
    fn a_silent_compositor_times_out_instead_of_hanging() {
        let fx = Fixture::new();
        let path = fx.dir().join("hypr.sock");
        let listener = UnixListener::bind(&path).expect("bind");

        // Accept, then hold the connection open without answering.
        let server = std::thread::spawn(move || {
            let (conn, _) = listener.accept().expect("accept");
            std::thread::sleep(Duration::from_secs(3));
            drop(conn);
        });

        let started = std::time::Instant::now();
        assert_eq!(request(&path, "monitors"), None);
        assert!(started.elapsed() < Duration::from_secs(2));
        server.join().expect("server thread");
    }

    #[test]
    fn socket_path_prefers_the_runtime_dir_and_rejects_odd_signatures() {
        let fx = Fixture::new();
        fx.write("run/hypr/abc_123/.socket.sock", "");

        let runtime = fx.dir().join("run");

        assert_eq!(
            socket_path(Some(&runtime), Path::new("abc_123")),
            Some(runtime.join("hypr/abc_123/.socket.sock"))
        );
        assert_eq!(socket_path(Some(&runtime), Path::new("../../etc")), None);
        assert_eq!(socket_path(Some(&runtime), Path::new("missing")), None);
    }
}
