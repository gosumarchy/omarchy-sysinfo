//! The collector thread.
//!
//! Collection reads hundreds of sysfs files, may ask `iw` or `omarchy`
//! questions, and calls `statvfs` on every mounted disk. Any of that can
//! stall (a hung mount, a wedged driver), and on the UI thread a stall froze
//! the screen: raw mode turns Ctrl+C into an ordinary byte, so the user could
//! not even quit. Here it only delays the next snapshot.

use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use crate::collect::{Collector, Host};
use crate::event::Event;

/// How often the machine is re-read without being asked.
const INTERVAL: Duration = Duration::from_secs(2);

/// The gap before the second reading. CPU usage is a delta between two
/// readings, so the first snapshot can only say "sampling"; a short first
/// wait makes real figures appear almost at once instead of after a full
/// interval.
const PRIME: Duration = Duration::from_millis(300);

/// What the UI can ask of the collector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Request {
    Refresh,
}

/// Start collecting on a thread of its own. Snapshots arrive on `events`;
/// the returned sender asks for an immediate refresh. The thread ends when
/// either side of either channel goes away.
pub(crate) fn spawn(host: Host, events: Sender<Event>) -> Sender<Request> {
    spawn_with(host, events, PRIME, INTERVAL)
}

fn spawn_with(
    host: Host,
    events: Sender<Event>,
    prime: Duration,
    interval: Duration,
) -> Sender<Request> {
    let (requests, incoming) = mpsc::channel();

    thread::spawn(move || {
        let mut collector = Collector::new(host);
        let mut wait = prime;

        loop {
            if events
                .send(Event::Snapshot(Box::new(collector.collect())))
                .is_err()
            {
                return;
            }

            match incoming.recv_timeout(wait) {
                // A held `r` queues many requests; one collection answers
                // them all.
                Ok(Request::Refresh) => while incoming.try_recv().is_ok() {},
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            wait = interval;
        }
    });

    requests
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::fixture::Fixture;

    const LONG: Duration = Duration::from_secs(3600);

    fn next_snapshot(events: &mpsc::Receiver<Event>) {
        match events.recv_timeout(Duration::from_secs(10)) {
            Ok(Event::Snapshot(_)) => {}
            other => panic!("expected a snapshot, got {other:?}"),
        }
    }

    #[test]
    fn a_snapshot_arrives_at_once_and_again_on_request() {
        let fx = Fixture::new();
        let (tx, events) = mpsc::channel();
        let refresh = spawn_with(fx.host(), tx, LONG, LONG);

        next_snapshot(&events);
        refresh.send(Request::Refresh).expect("worker alive");
        next_snapshot(&events);
    }

    #[test]
    fn the_second_snapshot_follows_the_priming_delay() {
        let fx = Fixture::new();
        let (tx, events) = mpsc::channel();
        let _refresh = spawn_with(fx.host(), tx, Duration::from_millis(10), LONG);

        next_snapshot(&events);
        next_snapshot(&events);
    }

    #[test]
    fn many_refresh_requests_collapse_into_one_collection() {
        let fx = Fixture::new();
        let (tx, events) = mpsc::channel();
        let refresh = spawn_with(fx.host(), tx, LONG, LONG);
        next_snapshot(&events);

        for _ in 0..50 {
            refresh.send(Request::Refresh).expect("worker alive");
        }
        next_snapshot(&events);

        // Anything beyond one or two more would mean the queue was replayed.
        let extra = std::iter::from_fn(|| events.recv_timeout(Duration::from_millis(200)).ok())
            .take(10)
            .count();
        assert!(extra <= 1, "{extra} extra snapshots");
    }

    #[test]
    fn the_worker_stops_when_the_ui_goes_away() {
        let fx = Fixture::new();
        let (tx, events) = mpsc::channel();
        let refresh = spawn_with(fx.host(), tx, LONG, LONG);
        next_snapshot(&events);

        drop(events);
        // The next send fails and the thread exits; the request channel then
        // reports the other side gone.
        let _ = refresh.send(Request::Refresh);
        std::thread::sleep(Duration::from_millis(200));
        assert!(refresh.send(Request::Refresh).is_err());
    }
}
