//! What the main loop waits for. The keyboard thread and the collector
//! thread both feed one channel, so the loop blocks in one place and reacts
//! to whichever arrives first.

use crate::collect::Snapshot;
use crate::input::Key;

#[derive(Debug)]
pub(crate) enum Event {
    Key(Key),
    /// Stdin closed: nobody is left to press `q`.
    InputClosed,
    /// A fresh description of the machine, and what made the worker take it.
    /// Boxed because it is far larger than a key, and every `Event` is the
    /// size of its largest variant.
    Snapshot(Box<Snapshot>, Trigger),
}

/// Why a snapshot was taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Trigger {
    /// The first reading, or the refresh interval ran out.
    Timer,
    /// The collection started after the user asked for a refresh, so it is
    /// the answer to that request. A snapshot already in flight when `r` was
    /// pressed is not.
    Request,
}
