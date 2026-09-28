//! Keyboard decoding with no crates. A background thread blocks on stdin and
//! sends decoded keys down the event channel; the main loop waits on that
//! channel alongside fresh snapshots.

use std::io::Read;
use std::sync::mpsc::Sender;
use std::time::Duration;

use crate::event::Event;

/// Ceiling on buffered input bytes. A terminal that opens an escape sequence and
/// never finishes it would otherwise grow the buffer for the life of the
/// process and stop decoding any key at all.
const MAX_PENDING: usize = 4096;

/// How long a lone ESC waits for the rest of a sequence.
///
/// A terminal writes an escape sequence in one go, but over ssh or a slow
/// pty it can arrive split, and an arrow key read as Esc-then-`[A` quit the
/// app. This is long enough to join a split sequence and short enough that
/// the Esc key still feels immediate.
const ESC_WAIT: Duration = Duration::from_millis(30);

const STDIN: i32 = 0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Key {
    Char(char),
    /// Alt held with a printable key, which terminals send as ESC then the
    /// key. Decoding it as Esc used to make any Alt chord quit the app.
    Alt(char),
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Enter,
    Backspace,
    Tab,
    Esc,
    CtrlC,
    Unknown,
}

/// Bytes read from stdin that do not yet add up to a whole key.
///
/// The size ceiling belongs to the type rather than to the caller: `extend` is
/// the only way in, so there is nowhere for the guard to be forgotten. Nothing
/// else needs to know that the ceiling exists.
#[derive(Default)]
struct InputBuffer {
    bytes: Vec<u8>,
}

impl InputBuffer {
    /// Append a freshly read chunk.
    ///
    /// Returns `Some(Key::Unknown)` if that pushed the buffer past
    /// [`MAX_PENDING`], which drops the bytes instead of decoding them:
    /// whatever is in there is no longer a sequence anybody typed, and holding
    /// on to it would mean never decoding a key again.
    fn extend(&mut self, chunk: &[u8]) -> Option<Key> {
        self.bytes.extend_from_slice(chunk);

        if self.bytes.len() > MAX_PENDING {
            self.bytes.clear();

            return Some(Key::Unknown);
        }

        None
    }

    /// Whether all that is buffered is one ESC, which may be the Esc key or
    /// the first byte of a sequence whose rest has not arrived yet.
    fn is_lone_escape(&self) -> bool {
        self.bytes == [0x1b]
    }

    /// Pull one key off the front, if there is a complete one.
    fn take_key(&mut self) -> Option<Key> {
        let first = *self.bytes.first()?;

        // A lone ESC is the Esc key: the reader has already given the rest
        // of a sequence `ESC_WAIT` to turn up.
        if first == 0x1b {
            if self.bytes.len() == 1 {
                self.bytes.clear();

                return Some(Key::Esc);
            }

            return self.parse_escape();
        }

        // UTF-8: figure out how long the character is before decoding it.
        let needed = utf8_len(first);
        if needed == 0 {
            // A continuation byte or 0xf8..=0xff with nothing valid to follow:
            // dropping it whole beats decoding it as a Latin-1 mojibake char.
            self.bytes.remove(0);

            return Some(Key::Unknown);
        }
        if needed > 1 {
            return self.take_multibyte(needed);
        }

        self.bytes.remove(0);
        Some(match first {
            0x0d | 0x0a => Key::Enter,
            0x09 => Key::Tab,
            0x7f | 0x08 => Key::Backspace,
            0x03 => Key::CtrlC,
            // Other control bytes (Ctrl+letter) are not bound to anything and
            // must not end up typed into the filter.
            0x00..=0x1f => Key::Unknown,
            other => Key::Char(char::from(other)),
        })
    }

    fn take_multibyte(&mut self, needed: usize) -> Option<Key> {
        // Validate the continuation bytes already buffered. If one of them is
        // not a continuation byte the sequence is definitively malformed, so
        // drop just the bad lead and resync -- otherwise we would sit waiting
        // for `needed` bytes and then swallow the real keystroke that followed
        // (a stray ESC-prefixed 0x28 used to eat the 'a' typed after it).
        let continuation = self.bytes.iter().skip(1).take(needed - 1);
        if continuation.clone().any(|b| !(0x80..=0xbf).contains(b)) {
            self.bytes.remove(0);

            return Some(Key::Unknown);
        }
        if self.bytes.len() < needed {
            // Wait for the rest of the character. A multi-byte char can land
            // across two reads, and discarding the partial bytes here used to
            // swallow the character entirely.
            return None;
        }

        let decoded = std::str::from_utf8(&self.bytes[..needed])
            .ok()
            .and_then(|s| s.chars().next());
        self.bytes.drain(..needed);

        Some(decoded.map_or(Key::Unknown, Key::Char))
    }

    /// ESC followed by at least one more byte: a CSI or SS3 sequence, an
    /// Alt chord, or an Esc press followed by something else.
    fn parse_escape(&mut self) -> Option<Key> {
        match self.bytes.get(1).copied() {
            Some(b'[') => self.parse_csi(),
            Some(b'O') => self.parse_ss3(),
            Some(byte @ 0x20..=0x7e) => {
                self.bytes.drain(..2);

                Some(Key::Alt(char::from(byte)))
            }
            // Two escapes are two Esc presses; the second decodes on its own.
            Some(0x1b) | None => {
                self.bytes.remove(0);

                Some(Key::Esc)
            }
            // Alt with Enter, Backspace, Tab or a Ctrl chord. Nothing binds
            // these, and reading them as Esc quit the app (or, in the
            // filter, threw the query away on Alt+Backspace).
            Some(0x00..=0x1f | 0x7f) => {
                self.bytes.drain(..2);

                Some(Key::Unknown)
            }
            // Alt with a non-ASCII key, `ESC ä`: the character after the ESC
            // is a whole UTF-8 sequence.
            Some(lead) => self.parse_alt_multibyte(lead),
        }
    }

    /// `ESC` followed by a UTF-8 lead byte: Alt with a non-ASCII key.
    fn parse_alt_multibyte(&mut self, lead: u8) -> Option<Key> {
        let needed = utf8_len(lead);
        if needed < 2 {
            // Not a character at all: drop the pair rather than call it Esc.
            self.bytes.drain(..2);

            return Some(Key::Unknown);
        }
        if self.bytes.len() < 1 + needed {
            // The rest of the character is still on its way.
            return None;
        }

        let decoded = std::str::from_utf8(&self.bytes[1..=needed])
            .ok()
            .and_then(|s| s.chars().next());
        self.bytes.drain(..=needed);

        Some(decoded.map_or(Key::Unknown, Key::Alt))
    }

    /// xterm style: `ESC [ <params> <final byte>`.
    fn parse_csi(&mut self) -> Option<Key> {
        let Some(offset) = self.bytes[2..]
            .iter()
            .position(|b| (0x40..=0x7e).contains(b))
        else {
            // Incomplete: keep the bytes and wait for the next read, since a
            // split CSI would otherwise lose the key entirely. The ceiling in
            // `extend` stops a never-finished sequence from wedging the
            // decoder.
            return None;
        };
        let end = 2 + offset;
        let params = String::from_utf8_lossy(&self.bytes[2..end]).into_owned();
        let final_byte = self.bytes[end];
        self.bytes.drain(..=end);

        Some(match final_byte {
            b'A' => Key::Up,
            b'B' => Key::Down,
            b'C' => Key::Right,
            b'D' => Key::Left,
            b'H' => Key::Home,
            b'F' => Key::End,
            b'~' => match params.split(';').next() {
                Some("1" | "7") => Key::Home,
                Some("4" | "8") => Key::End,
                Some("5") => Key::PageUp,
                Some("6") => Key::PageDown,
                // Insert, Delete, F-keys and bracketed paste are not bound.
                Some(_) | None => Key::Unknown,
            },
            // Every other final byte is a key or a report nothing binds.
            _ => Key::Unknown,
        })
    }

    /// Application cursor keys: `ESC O <final byte>`.
    fn parse_ss3(&mut self) -> Option<Key> {
        let final_byte = *self.bytes.get(2)?;
        self.bytes.drain(..3);

        Some(match final_byte {
            b'A' => Key::Up,
            b'B' => Key::Down,
            b'C' => Key::Right,
            b'D' => Key::Left,
            b'H' => Key::Home,
            b'F' => Key::End,
            // F1-F4 and keypad keys are not bound.
            _ => Key::Unknown,
        })
    }
}

/// Spawn the reader thread. It sends `InputClosed` and exits when stdin
/// closes or the receiving end is dropped.
pub(crate) fn spawn(events: Sender<Event>) {
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut pending = InputBuffer::default();
        let mut buf = [0u8; 64];

        loop {
            let n = match stdin.read(&mut buf) {
                Ok(0) | Err(_) => {
                    let _ = events.send(Event::InputClosed);

                    return;
                }
                Ok(n) => n,
            };

            // A dropped buffer reports itself as a key before the drain below,
            // so the user sees the input recover rather than the app hang.
            if let Some(key) = pending.extend(&buf[..n])
                && events.send(Event::Key(key)).is_err()
            {
                return;
            }

            let more_coming = || crate::sys::wait_readable(STDIN, ESC_WAIT);
            if !drain(&mut pending, more_coming, |key| {
                events.send(Event::Key(key)).is_ok()
            }) {
                return;
            }
        }
    });
}

/// Decode every whole key at the front of the buffer and hand it to `emit`.
///
/// An ESC left on its own, whether it was the whole read or the tail of one
/// (`ESC [ A ESC` with the next `[ A` still in flight), first gets
/// `more_coming` to say whether more input is about to arrive; if it is,
/// the ESC stays buffered for the next read. Checking only reads that were
/// a lone ESC let a held arrow key over ssh decode as Esc and quit.
///
/// Returns false once `emit` reports the receiver is gone.
fn drain(
    pending: &mut InputBuffer,
    mut more_coming: impl FnMut() -> bool,
    mut emit: impl FnMut(Key) -> bool,
) -> bool {
    loop {
        if pending.is_lone_escape() && more_coming() {
            return true;
        }
        let Some(key) = pending.take_key() else {
            return true;
        };
        if !emit(key) {
            return false;
        }
    }
}

/// How many bytes `first` announces, or 0 when it cannot start a character.
/// 0xc0/0xc1 (overlong) and 0xf5..=0xff (beyond U+10FFFF) are rejected here;
/// `from_utf8` would catch them too, but rejecting early keeps the table honest.
fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        0x80..=0xc1 | 0xf5..=0xff => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed bytes in the given chunks and collect every key the decoder emits,
    /// mirroring how the reader thread appends each `read()` and then drains.
    fn feed(chunks: &[&[u8]]) -> Vec<Key> {
        let mut pending = InputBuffer::default();
        let mut out = Vec::new();
        for chunk in chunks {
            out.extend(pending.extend(chunk));
            while let Some(key) = pending.take_key() {
                out.push(key);
            }
        }
        out
    }

    fn one(bytes: &[u8]) -> Key {
        let keys = feed(&[bytes]);
        assert_eq!(
            keys.len(),
            1,
            "expected exactly one key from {bytes:02x?}, got {keys:?}"
        );
        keys[0]
    }

    // ---- plain ASCII -----------------------------------------------------

    #[test]
    fn decodes_printable_ascii() {
        assert_eq!(one(b"a"), Key::Char('a'));
        assert_eq!(one(b"Z"), Key::Char('Z'));
        assert_eq!(one(b" "), Key::Char(' '));
        assert_eq!(one(b"~"), Key::Char('~'));
    }

    #[test]
    fn decodes_control_keys() {
        // Terminals send CR for Enter; some send LF. Both must work.
        assert_eq!(one(b"\r"), Key::Enter);
        assert_eq!(one(b"\n"), Key::Enter);
        assert_eq!(one(b"\t"), Key::Tab);
        assert_eq!(one(b"\x7f"), Key::Backspace);
        assert_eq!(one(b"\x08"), Key::Backspace);
        assert_eq!(one(b"\x03"), Key::CtrlC);
        assert_eq!(one(b"\x1b"), Key::Esc);
    }

    #[test]
    fn a_burst_of_typing_decodes_in_order() {
        let keys = feed(&[b"q", b"j", b"k", b"/", b"?"]);
        assert_eq!(
            keys,
            vec![
                Key::Char('q'),
                Key::Char('j'),
                Key::Char('k'),
                Key::Char('/'),
                Key::Char('?')
            ]
        );
    }

    #[test]
    fn many_keys_in_one_read_all_decode() {
        let keys = feed(&[b"abcdefghij\r\tq"]);
        assert_eq!(keys.len(), 13);
        assert_eq!(keys.last(), Some(&Key::Char('q')));
    }

    // ---- CSI arrows ------------------------------------------------------

    #[test]
    fn decodes_csi_cursor_keys() {
        assert_eq!(one(b"\x1b[A"), Key::Up);
        assert_eq!(one(b"\x1b[B"), Key::Down);
        assert_eq!(one(b"\x1b[C"), Key::Right);
        assert_eq!(one(b"\x1b[D"), Key::Left);
        assert_eq!(one(b"\x1b[H"), Key::Home);
        assert_eq!(one(b"\x1b[F"), Key::End);
    }

    #[test]
    fn decodes_csi_tilde_keys() {
        assert_eq!(one(b"\x1b[1~"), Key::Home);
        assert_eq!(one(b"\x1b[7~"), Key::Home);
        assert_eq!(one(b"\x1b[4~"), Key::End);
        assert_eq!(one(b"\x1b[8~"), Key::End);
        assert_eq!(one(b"\x1b[5~"), Key::PageUp);
        assert_eq!(one(b"\x1b[6~"), Key::PageDown);
    }

    #[test]
    fn decodes_modified_csi_keys_by_ignoring_modifiers() {
        // Ctrl/Shift variants arrive as ESC [ 1 ; 5 A. The final byte still
        // names the key, so modifiers must not turn it into Unknown.
        assert_eq!(one(b"\x1b[1;5A"), Key::Up);
        assert_eq!(one(b"\x1b[1;2C"), Key::Right);
        assert_eq!(one(b"\x1b[3;5~"), Key::Unknown, "3 is not a tilde key");
    }

    #[test]
    fn unknown_csi_final_bytes_decode_to_unknown_not_a_char() {
        assert_eq!(one(b"\x1b[Z"), Key::Unknown);
        // A Device Status Report reply should not look like a keypress.
        assert_eq!(one(b"\x1b[?1;2c"), Key::Unknown);
        // Unrecognised tilde numbers must not alias onto Home/End/PageUp.
        assert_eq!(one(b"\x1b[2~"), Key::Unknown);
        assert_eq!(one(b"\x1b[9~"), Key::Unknown);
        assert_eq!(one(b"\x1b[200~"), Key::Unknown);
    }

    #[test]
    fn decodes_ss3_application_cursor_keys() {
        assert_eq!(one(b"\x1bOA"), Key::Up);
        assert_eq!(one(b"\x1bOD"), Key::Left);
        assert_eq!(one(b"\x1bOF"), Key::End);
        assert_eq!(one(b"\x1bOZ"), Key::Unknown);
    }

    // ---- sequences split across reads ------------------------------------

    #[test]
    fn a_lone_esc_alone_in_a_read_is_always_the_esc_key() {
        // Deliberate design trade-off, documented by the code: an ESC sitting
        // by itself is the Esc keypress and is delivered immediately. Waiting
        // for a possible continuation would make Esc feel laggy, since the
        // terminal cannot tell us whether more bytes are coming.
        assert_eq!(feed(&[b"\x1b"]), vec![Key::Esc]);
    }

    #[test]
    fn a_csi_split_after_the_bracket_waits_for_the_final_byte() {
        // Once ESC [ has arrived together we know a sequence is in flight, so
        // the decoder waits for the final byte instead of giving up on it.
        let keys = feed(&[b"\x1b[", b"A"]);
        assert_eq!(keys, vec![Key::Up], "split arrow must not be lost");

        let keys = feed(&[b"\x1b[", b"5", b"~"]);
        assert_eq!(keys, vec![Key::PageUp]);

        let keys = feed(&[b"\x1b[", b"1", b";", b"5", b"C"]);
        assert_eq!(keys, vec![Key::Right]);
    }

    #[test]
    fn an_ss3_split_after_the_o_waits_for_the_final_byte() {
        let keys = feed(&[b"\x1bO", b"B"]);
        assert_eq!(keys, vec![Key::Down]);
    }

    #[test]
    fn a_typed_arrow_immediately_after_split_keeps_both_keys() {
        // The buffer must not swallow the following keystrokes.
        let keys = feed(&[b"\x1b[", b"B", b"q"]);
        assert_eq!(keys, vec![Key::Down, Key::Char('q')]);
    }

    // ---- UTF-8 -----------------------------------------------------------

    #[test]
    fn decodes_multibyte_utf8() {
        // 'é' (2 bytes), '→' (3), '🎉' (4)
        assert_eq!(one("é".as_bytes()), Key::Char('é'));
        assert_eq!(one("→".as_bytes()), Key::Char('→'));
        assert_eq!(one("🎉".as_bytes()), Key::Char('🎉'));
    }

    #[test]
    fn a_multibyte_char_split_across_reads_is_not_lost() {
        // The bug this guards: the first read produced a partial char, the
        // decoder cleared the buffer and reported Unknown, so the character
        // vanished and its tail was later decoded as mojibake.
        let bytes = "é".as_bytes();
        assert_eq!(feed(&[&bytes[..1], &bytes[1..]]), vec![Key::Char('é')]);

        let arrow = "🎉".as_bytes();
        assert_eq!(feed(&[&arrow[..2], &arrow[2..]]), vec![Key::Char('🎉')]);
    }

    #[test]
    fn a_multibyte_char_followed_by_ascii_in_one_read_decodes_both() {
        // The continuation check used to look at every buffered byte, not
        // just the character's own, so the 'a' made the 'é' "malformed".
        assert_eq!(
            feed(&["éa".as_bytes()]),
            vec![Key::Char('é'), Key::Char('a')]
        );
    }

    #[test]
    fn multibyte_and_ascii_mix_keeps_order() {
        let keys = feed(&["a".as_bytes(), "é".as_bytes(), b"z"]);
        assert_eq!(keys, vec![Key::Char('a'), Key::Char('é'), Key::Char('z')]);
    }

    #[test]
    fn a_stray_continuation_byte_is_unknown_not_mojibake() {
        // 0x82 alone is not a character. It used to decode to '‚' (a Latin-1
        // reading of the byte) and then be searched for as a real keystroke.
        assert_eq!(one(b"\x82"), Key::Unknown);
        assert_eq!(one(b"\x80"), Key::Unknown);
        assert_eq!(one(b"\xbf"), Key::Unknown);
    }

    #[test]
    fn invalid_lead_bytes_are_dropped_without_derailing_the_stream() {
        // 0xff and 0xf8 can never start a character; the decoder must skip the
        // bad byte and still deliver the good key behind it.
        assert_eq!(feed(&[b"\xff", b"a"]), vec![Key::Unknown, Key::Char('a')]);
        assert_eq!(feed(&[b"\xf8", b"a"]), vec![Key::Unknown, Key::Char('a')]);
    }

    #[test]
    fn a_malformed_multibyte_sequence_does_not_swallow_the_next_keystroke() {
        // 0xe2 announces a 3-byte char but is followed by ASCII. The decoder
        // must report the bad byte as Unknown and still deliver the 'a',
        // rather than consuming three bytes and eating the keystroke.
        assert_eq!(feed(&[b"\xe2", b"a"]), vec![Key::Unknown, Key::Char('a')]);
        assert_eq!(
            feed(&[b"\xe2\x28", b"a"]),
            vec![Key::Unknown, Key::Char('('), Key::Char('a')]
        );
        assert_eq!(
            feed(&[b"\xc3", b"\xff", b"z"]),
            vec![Key::Unknown, Key::Unknown, Key::Char('z')]
        );
    }

    #[test]
    fn utf8_len_rejects_overlong_and_out_of_range_leads() {
        assert_eq!(utf8_len(b'a'), 1);
        assert_eq!(utf8_len(0xc2), 2);
        assert_eq!(utf8_len(0xe2), 3);
        assert_eq!(utf8_len(0xf0), 4);
        assert_eq!(utf8_len(0xf4), 4);
        // Overlong 2-byte leads and beyond-U+10FFFF leads are invalid.
        assert_eq!(utf8_len(0xc0), 0);
        assert_eq!(utf8_len(0xc1), 0);
        assert_eq!(utf8_len(0xf5), 0);
        assert_eq!(utf8_len(0xff), 0);
        // Continuation bytes cannot start a character.
        assert_eq!(utf8_len(0x80), 0);
        assert_eq!(utf8_len(0x9f), 0);
    }

    // ---- alt / esc combinations -----------------------------------------

    #[test]
    fn esc_followed_by_a_printable_key_is_an_alt_chord() {
        // Alt+x arrives as ESC x. Decoding it as Esc then 'x' made every Alt
        // chord quit the app.
        assert_eq!(feed(&[b"\x1bx"]), vec![Key::Alt('x')]);
        assert_eq!(feed(&[b"\x1bq"]), vec![Key::Alt('q')]);
    }

    #[test]
    fn alt_with_a_control_key_is_unbound_rather_than_esc() {
        // Alt+Enter, Alt+Backspace: reading them as Esc quit the app, and in
        // the filter Alt+Backspace threw the whole query away.
        assert_eq!(feed(&[b"\x1b\r"]), vec![Key::Unknown]);
        assert_eq!(feed(&[b"\x1b\x7f"]), vec![Key::Unknown]);
    }

    #[test]
    fn two_escapes_are_two_esc_presses() {
        assert_eq!(feed(&[b"\x1b\x1b"]), vec![Key::Esc, Key::Esc]);
    }

    #[test]
    fn alt_with_a_non_ascii_key_is_an_alt_chord() {
        assert_eq!(feed(&["\x1bä".as_bytes()]), vec![Key::Alt('ä')]);
        assert_eq!(
            feed(&["\x1b→q".as_bytes()]),
            vec![Key::Alt('→'), Key::Char('q')]
        );
    }

    #[test]
    fn an_alt_chord_split_inside_its_character_waits_for_the_rest() {
        let bytes = "\x1bä".as_bytes();
        assert_eq!(feed(&[&bytes[..2], &bytes[2..]]), vec![Key::Alt('ä')]);
    }

    #[test]
    fn esc_before_a_stray_continuation_byte_is_dropped_not_esc() {
        assert_eq!(feed(&[b"\x1b\x80a"]), vec![Key::Unknown, Key::Char('a')]);
    }

    fn drain_all(pending: &mut InputBuffer, more_coming: bool) -> Vec<Key> {
        let mut keys = Vec::new();
        drain(
            pending,
            || more_coming,
            |key| {
                keys.push(key);
                true
            },
        );
        keys
    }

    #[test]
    fn an_esc_at_the_end_of_a_read_waits_for_the_rest_of_its_sequence() {
        // A held arrow key over ssh, split as "ESC [ A ESC" then "[ A".
        let mut pending = InputBuffer::default();
        pending.extend(b"\x1b[A\x1b");
        assert_eq!(drain_all(&mut pending, true), vec![Key::Up]);

        pending.extend(b"[A");
        assert_eq!(drain_all(&mut pending, true), vec![Key::Up]);
    }

    #[test]
    fn an_esc_with_nothing_following_is_the_esc_key() {
        let mut pending = InputBuffer::default();
        pending.extend(b"\x1b[A\x1b");

        assert_eq!(drain_all(&mut pending, false), vec![Key::Up, Key::Esc]);
    }

    #[test]
    fn drain_stops_when_the_receiver_is_gone() {
        let mut pending = InputBuffer::default();
        pending.extend(b"abc");

        assert!(!drain(&mut pending, || false, |_| false));
    }

    #[test]
    fn a_lone_escape_is_recognised_as_such() {
        let mut pending = InputBuffer::default();
        pending.extend(b"\x1b");
        assert!(pending.is_lone_escape());
        pending.extend(b"[");
        assert!(!pending.is_lone_escape());
    }

    #[test]
    fn unbound_control_bytes_are_not_typed_characters() {
        // Ctrl+W, Ctrl+Z and NUL used to arrive as Key::Char and be typed
        // into the filter, then drawn raw onto the terminal.
        assert_eq!(one(b"\x17"), Key::Unknown);
        assert_eq!(one(b"\x1a"), Key::Unknown);
        assert_eq!(one(b"\x00"), Key::Unknown);
    }

    // ---- buffer safety ---------------------------------------------------

    #[test]
    fn an_unterminated_sequence_cannot_wedge_the_decoder() {
        // A terminal that opens a CSI and never finishes it used to grow the
        // buffer forever. The ceiling must drop it and resynchronise, and it
        // has to fire on the way in, not on the way out: nothing ever calls
        // `take_key` again once the stream is that far gone.
        let mut pending = InputBuffer::default();
        assert_eq!(pending.extend(b"\x1b["), None);
        let dropped = pending.extend(&[b'1'; MAX_PENDING + 10]);
        assert_eq!(dropped, Some(Key::Unknown));
        assert!(
            pending.bytes.is_empty(),
            "buffer must be dropped, not retained"
        );

        // And the very next key still decodes.
        assert_eq!(pending.extend(b"a"), None);
        assert_eq!(pending.take_key(), Some(Key::Char('a')));
    }

    #[test]
    fn the_buffer_is_only_dropped_once_it_is_over_the_ceiling() {
        let mut pending = InputBuffer::default();
        assert_eq!(pending.extend(&[b'1'; MAX_PENDING]), None, "at the ceiling");
        assert_eq!(pending.bytes.len(), MAX_PENDING);
    }

    #[test]
    fn take_key_on_an_empty_buffer_returns_none() {
        let mut pending = InputBuffer::default();
        assert_eq!(pending.take_key(), None);
    }

    #[test]
    fn every_key_presses_in_one_line_decode_in_sequence() {
        // The keys the app actually binds, as one paste-like burst.
        let keys = feed(&[b"jg/?\x1b[A\x1b[B\x1b[C\x1b[D\x1b[5~\x1b[6~\t\r\x1b[H\x1b[F"]);
        assert_eq!(
            keys,
            vec![
                Key::Char('j'),
                Key::Char('g'),
                Key::Char('/'),
                Key::Char('?'),
                Key::Up,
                Key::Down,
                Key::Right,
                Key::Left,
                Key::PageUp,
                Key::PageDown,
                Key::Tab,
                Key::Enter,
                Key::Home,
                Key::End,
            ]
        );
    }
}
