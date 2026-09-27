//! omarchy-sysinfo — describe this machine, section by section.
//!
//! No dependencies: the terminal, the keyboard, and every `/proc` and `/sys`
//! parser here are written against the standard library only.

mod app;
mod cli;
mod collect;
mod error;
mod input;
mod report;
mod term;
mod ui;

use std::time::Duration;

use error::Result;

/// Hand the command line to `cli`, then run the TUI if it wants one.
fn main() -> Result<()> {
    if let Some(result) = cli::handle_args() {
        return result;
    }

    run()
}

/// Draw the report, wait for a key or a refresh, and repeat until the user
/// quits.
fn run() -> Result<()> {
    let mut terminal = term::Terminal::enter()?;
    let keys = input::spawn();
    let mut app = app::App::new();
    let palette = ui::theme::Palette::from_omarchy();

    let mut redraw = true;
    loop {
        if redraw {
            terminal.refresh_size();
            let (width, height) = terminal.size();
            let mut buffer = term::Buffer::new(
                width,
                height,
                term::Style::new(term::Color::Default).on(palette.background),
            );
            ui::draw(&mut buffer, &app, &palette);
            terminal.draw(&buffer)?;
            redraw = false;
        }

        if let Some(key) = input::next_key(&keys, Duration::from_millis(250)) {
            redraw = true;
            // Inside a filter, `r` is a character, not a refresh.
            let batch = if app.filter.active {
                vec![key]
            } else {
                input::coalesce_refresh(key, &keys)
            };
            for key in batch {
                app.on_key(key);
            }
        }

        if app.tick() {
            redraw = true;
        }

        if app.should_quit {
            return Ok(());
        }
    }
}
