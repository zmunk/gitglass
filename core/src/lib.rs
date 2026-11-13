pub mod actions;
pub mod config;
pub mod hints;

use std::io;

use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal, TerminalOptions, Viewport};

/// RAII guard that restores the terminal on drop (including on panic).
pub struct TerminalGuard {
    alternate_screen: bool,
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        if self.alternate_screen {
            let _ = execute!(io::stderr(), LeaveAlternateScreen);
        }
        let _ = execute!(io::stderr(), crossterm::cursor::Show);
    }
}

/// Initialise the terminal for the TUI application.
///
/// Enables raw mode, enters the alternate screen, installs a panic hook that
/// restores the terminal, and returns a `Terminal` and a `TerminalGuard`.
pub fn init_terminal() -> io::Result<(Terminal<CrosstermBackend<io::Stderr>>, TerminalGuard)> {
    init_terminal_mode(None)
}

/// Initialise a content-sized viewport in the main terminal buffer.
pub fn init_inline_terminal(
    height: u16,
) -> io::Result<(Terminal<CrosstermBackend<io::Stderr>>, TerminalGuard)> {
    init_terminal_mode(Some(height))
}

fn init_terminal_mode(
    inline_height: Option<u16>,
) -> io::Result<(Terminal<CrosstermBackend<io::Stderr>>, TerminalGuard)> {
    enable_raw_mode()?;
    let alternate_screen = inline_height.is_none();
    let guard = TerminalGuard { alternate_screen };
    if alternate_screen {
        execute!(io::stderr(), EnterAlternateScreen)?;
    }

    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        if alternate_screen {
            let _ = execute!(io::stderr(), LeaveAlternateScreen);
        }
        let _ = execute!(io::stderr(), crossterm::cursor::Show);
        original_hook(info);
    }));

    let mut terminal = Terminal::with_options(
        CrosstermBackend::new(io::stderr()),
        TerminalOptions {
            viewport: inline_height.map_or(Viewport::Fullscreen, Viewport::Inline),
        },
    )?;
    terminal.clear()?;
    Ok((terminal, guard))
}

/// Resize an inline panel, or start it below a completed external command.
pub fn resize_inline_terminal(
    terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
    height: u16,
    after_command: bool,
) -> io::Result<()> {
    if !after_command {
        let area = terminal.get_frame().area();
        terminal.clear()?;
        execute!(terminal.backend_mut(), crossterm::cursor::MoveTo(0, area.y))?;
    }
    *terminal = Terminal::with_options(
        CrosstermBackend::new(io::stderr()),
        TerminalOptions {
            viewport: Viewport::Inline(height),
        },
    )?;
    Ok(())
}

/// Remove inline hints and leave the cursor immediately below the visible list.
pub fn finish_inline_terminal(
    terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
    list_height: u16,
) -> io::Result<()> {
    let area = terminal.get_frame().area();
    if list_height < area.height {
        terminal.set_cursor_position((area.x, area.y + list_height))?;
        execute!(
            terminal.backend_mut(),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::FromCursorDown)
        )?;
    } else {
        terminal.set_cursor_position((area.x, area.bottom().saturating_sub(1)))?;
        execute!(terminal.backend_mut(), crossterm::style::Print("\r\n"))?;
    }
    Ok(())
}

/// Suspend the TUI so an external command can use the terminal.
///
/// When `leave_alternate_screen` is `true` (the default), the alternate
/// screen is left so line-mode command output appears on the main terminal
/// buffer and survives after the TUI exits.  Set it to `false` when the
/// child process is itself a TUI application that manages its own alternate
/// screen — this avoids a distracting flash as control transitions.
pub fn suspend_terminal<W: io::Write>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    leave_alternate_screen: bool,
) -> io::Result<()> {
    terminal.show_cursor()?;
    disable_raw_mode()?;
    if leave_alternate_screen {
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            crossterm::cursor::Show
        )?;
    }
    Ok(())
}

/// Restore the TUI after an external command has finished.
pub fn restore_terminal<W: io::Write>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
) -> io::Result<()> {
    enable_raw_mode()?;
    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
    terminal.clear()?;
    Ok(())
}

/// Install signal handlers so that Ctrl-C / Ctrl-\\ interrupt the child process
/// rather than the parent. The handlers are reset on exec, so the shell and its
/// children retain normal signal behaviour.
pub fn prepare_signals() -> io::Result<()> {
    #[cfg(unix)]
    {
        use signal_hook::consts::{SIGINT, SIGQUIT};
        use std::sync::{atomic::AtomicBool, Arc};

        let interrupted = Arc::new(AtomicBool::new(false));
        for signal in [SIGINT, SIGQUIT] {
            signal_hook::flag::register(signal, Arc::clone(&interrupted))?;
        }
    }
    Ok(())
}
