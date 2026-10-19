use std::io;

use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

/// RAII guard that restores the terminal on drop (including on panic).
pub struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stderr(), LeaveAlternateScreen, crossterm::cursor::Show);
    }
}

/// Initialise the terminal for the TUI application.
///
/// Enables raw mode, enters the alternate screen, installs a panic hook that
/// restores the terminal, and returns a `Terminal` and a `TerminalGuard`.
pub fn init_terminal() -> io::Result<(Terminal<CrosstermBackend<io::Stderr>>, TerminalGuard)> {
    enable_raw_mode()?;
    let guard = TerminalGuard;
    execute!(io::stderr(), EnterAlternateScreen)?;

    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stderr(), LeaveAlternateScreen, crossterm::cursor::Show);
        original_hook(info);
    }));

    let mut terminal = Terminal::new(CrosstermBackend::new(io::stderr()))?;
    terminal.clear()?;
    Ok((terminal, guard))
}

/// Suspend the TUI so an external command can use the terminal.
///
/// When `leave_alternate_screen` is `true` (the default), the alternate
/// screen is left so line-mode command output appears on the main terminal
/// buffer and survives after the TUI exits.  Set it to `false` when the
/// child process is itself a TUI application that manages its own alternate
/// screen — this avoids a distracting flash as control transitions.
pub fn suspend_terminal(
    terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
    leave_alternate_screen: bool,
) -> io::Result<()> {
    terminal.show_cursor()?;
    disable_raw_mode()?;
    if leave_alternate_screen {
        execute!(io::stderr(), LeaveAlternateScreen, crossterm::cursor::Show)?;
    }
    Ok(())
}

/// Restore the TUI after an external command has finished.
pub fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stderr>>) -> io::Result<()> {
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
