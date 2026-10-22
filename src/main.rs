use std::{env, ffi::OsString, path::PathBuf, process::ExitCode};

use anyhow::{bail, Context, Result};
use gitglass_core::{init_terminal, prepare_signals};

const USAGE: &str = "\
Usage: gitglass [command] [args]

Commands:
  status            Show Git status with configurable keybindings
  diff <file>       Interactively stage/unstage content changes in a file
  log               Interactive Git log viewer with branch filtering
  help              Show this help message

With no command, runs the configured default_command, or shows help.
";

fn run() -> Result<()> {
    let mut args: Vec<OsString> = env::args_os().skip(1).collect();

    if args.is_empty() {
        let default = gitglass_core::config::load_default_command()
            .context("Cannot load gitglass configuration")?;
        if let Some(command) = default {
            args.push(OsString::from(command.as_str()));
        }
    }

    if args.is_empty()
        || args
            .first()
            .is_some_and(|a| a == "--help" || a == "-h" || a == "help")
    {
        eprint!("{USAGE}");
        return Ok(());
    }

    let command = args.remove(0);
    match command.to_str() {
        Some("status") => {
            if !args.is_empty() {
                bail!("gitglass status takes no arguments (got {args:?})");
            }

            // Load config *before* terminal init so parse errors print normally.
            let bindings =
                gitglass_core::config::load().context("Cannot load gitglass configuration")?;

            prepare_signals().context("Cannot prepare signal handlers")?;

            let (mut terminal, _guard) = init_terminal().context("Cannot initialise terminal")?;

            gitglass_status::run(&mut terminal, &bindings)
                .context("Status TUI exited with an error")?;

            Ok(())
        }
        Some("diff") => {
            if args.len() != 1 {
                bail!("gitglass diff requires exactly one file argument (got {args:?})");
            }

            let file_path = PathBuf::from(&args[0]);
            let cwd = env::current_dir()
                .context("Cannot determine current working directory")?
                .canonicalize()
                .context("Invocation directory is unavailable")?;

            // Open the repo *before* terminal init so validation errors print normally.
            let repo = gitglass_diff::Repo::open(&cwd, &file_path)
                .context("Cannot open repository context")?;
            let snapshot = repo.cached_snapshot();
            let file_name = args[0].to_string_lossy().into_owned();

            // We don't need signal handlers for the diff subcommand (no external
            // commands are launched), but we install them anyway for consistency.
            prepare_signals().context("Cannot prepare signal handlers")?;

            let (mut terminal, _guard) = init_terminal().context("Cannot initialise terminal")?;

            gitglass_diff::run(&mut terminal, repo, snapshot, &file_name)
                .context("Diff TUI exited with an error")?;

            Ok(())
        }
        Some("log") => {
            if !args.is_empty() {
                bail!("gitglass log takes no arguments (got {args:?})");
            }

            gitglass_log::run()
                .map_err(|e| anyhow::anyhow!("Log TUI exited with an error: {e:#}"))?;

            Ok(())
        }
        Some(other) => {
            bail!("Unknown command: {other}\n\n{USAGE}");
        }
        None => {
            bail!("Command must be valid UTF-8\n\n{USAGE}");
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("gitglass: {error:#}");
            ExitCode::FAILURE
        }
    }
}
