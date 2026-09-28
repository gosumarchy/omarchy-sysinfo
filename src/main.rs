//! omarchy-sysinfo — describe this machine, section by section.
//!
//! No dependencies: the terminal, the keyboard, and every `/proc` and `/sys`
//! parser here are written against the standard library only. The few
//! foreign calls it needs live in [`sys`].

mod app;
mod cli;
mod collect;
mod error;
mod event;
mod input;
mod report;
mod sys;
mod term;
mod text;
mod ui;
mod worker;

use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use app::{Action, App};
use cli::Command;
use collect::Host;
use error::{Error, Result};
use event::Event;
use report::Identifiers;

/// Exit status for a usage mistake, matching the usual CLI convention.
const USAGE_EXIT: u8 = 2;

/// How often the loop wakes without an event, to expire the status line and
/// notice a resize.
const TICK: Duration = Duration::from_millis(250);

fn main() -> ExitCode {
    let command = match cli::parse(std::env::args_os().skip(1)) {
        Ok(command) => command,
        Err(usage) => {
            complain(&usage);

            return ExitCode::from(USAGE_EXIT);
        }
    };

    let result = match command {
        Command::Tui => tui(),
        Command::Plain(identifiers) => report::print(identifiers),
        Command::Help => say(cli::HELP),
        Command::Version => say(&format!("omarchy-sysinfo {}", env!("CARGO_PKG_VERSION"))),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            complain(&e);

            ExitCode::FAILURE
        }
    }
}

fn say(text: &str) -> Result<()> {
    let mut out = io::stdout().lock();

    match writeln!(out, "{text}") {
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        other => Ok(other?),
    }
}

fn complain(error: &dyn std::fmt::Display) {
    let _ = writeln!(io::stderr().lock(), "omarchy-sysinfo: {error}");
}

/// The interactive view, or the plain report when there is no terminal to
/// draw on (`omarchy-sysinfo | less`).
fn tui() -> Result<()> {
    if !io::stdout().is_terminal() {
        return report::print(Identifiers::Hide);
    }
    if !io::stdin().is_terminal() {
        return Err(Error::NotATerminal);
    }

    let host = Host::live();
    let palette = ui::theme::Palette::from_omarchy(&host);
    let (events, incoming) = mpsc::channel();
    let refresh = worker::spawn(host, events.clone());

    // Raw mode first, so the keyboard thread never sees a cooked line.
    let mut terminal = term::Terminal::enter()?;
    input::spawn(events);

    let mut app = App::new();
    let mut size = (0, 0);
    let mut redraw = true;

    loop {
        terminal.refresh_size();
        if terminal.size() != size {
            size = terminal.size();
            redraw = true;
        }

        if redraw {
            let (width, height) = size;
            app.set_viewport(ui::detail_rows(height));
            let mut buffer = term::Buffer::new(
                width,
                height,
                term::Style::new(term::Color::Default).on(palette.background),
            );
            ui::draw(&mut buffer, &app, &palette);
            terminal.draw(&buffer)?;
            redraw = false;
        }

        match incoming.recv_timeout(TICK) {
            Ok(Event::Key(key)) => {
                redraw = true;
                match app.on_key(key) {
                    Action::Quit => return Ok(()),
                    Action::Refresh => {
                        // A worker that has died cannot refresh; the stale
                        // screen is the most honest thing left to show.
                        let _ = refresh.send(worker::Request::Refresh);
                    }
                    Action::Nothing => {}
                }
            }
            Ok(Event::Snapshot(snapshot, trigger)) => {
                app.on_snapshot(*snapshot, trigger);
                redraw = true;
            }
            Ok(Event::InputClosed) | Err(RecvTimeoutError::Disconnected) => return Ok(()),
            Err(RecvTimeoutError::Timeout) => {}
        }

        redraw |= app.tick();
    }
}
