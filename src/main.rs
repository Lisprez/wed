mod app;
mod render;
mod width;

use app::{App, AppOutcome};
use crossterm::{
    event::{self, Event, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use render::Renderer;
use std::io::{self, stdout};
use std::panic;
use std::path::PathBuf;

struct RawModeGuard;

impl RawModeGuard {
    fn new() -> io::Result<Self> {
        enable_raw_mode()?;
        if let Err(error) = stdout().execute(EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = stdout().execute(LeaveAlternateScreen);
    }
}

fn main() -> io::Result<()> {
    let _guard = RawModeGuard::new()?;

    panic::set_hook(Box::new(|info| {
        let _ = disable_raw_mode();
        let _ = stdout().execute(LeaveAlternateScreen);
        eprintln!("Fatal error: {:?}", info);
    }));

    let path = std::env::args_os().nth(1).map(PathBuf::from);
    let mut app = App::new(path)?;
    let mut renderer = Renderer::new()?;
    app.set_viewport_height(renderer.text_height());
    renderer.render(&app)?;

    // Block until an event arrives rather than polling on a timer: an idle
    // editor should cost nothing, and terminal resizes still arrive because
    // crossterm wakes the blocked read on SIGWINCH.
    loop {
        // Block until an event arrives rather than polling on a timer: an idle
        // editor should cost nothing, and terminal resizes still arrive because
        // crossterm wakes the blocked read on SIGWINCH.
        let mut quit = false;
        handle_event(&mut app, &mut renderer, event::read()?, &mut quit)?;
        // Drain whatever has already queued: a burst of keys renders as one
        // frame rather than one frame per key.
        while !quit && event::poll(std::time::Duration::ZERO)? {
            handle_event(&mut app, &mut renderer, event::read()?, &mut quit)?;
        }
        if quit {
            break;
        }
        app.set_viewport_height(renderer.text_height());
        renderer.render(&app)?;
    }

    Ok(())
}

fn handle_event(
    app: &mut App,
    renderer: &mut Renderer,
    event: Event,
    quit: &mut bool,
) -> io::Result<()> {
    match event {
        Event::Key(key) => {
            if key.kind == KeyEventKind::Release {
                return Ok(());
            }
            if app.handle_key(key)? == AppOutcome::Quit {
                *quit = true;
                return Ok(());
            }
            app.set_viewport_height(renderer.text_height());
        }
        Event::Resize(width, height) => {
            renderer.resize(width, height);
            app.set_viewport_height(renderer.text_height());
        }
        _ => {}
    }
    Ok(())
}
