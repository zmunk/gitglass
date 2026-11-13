use std::{env, ffi::OsString, path::PathBuf, process::ExitCode};

use anyhow::{bail, Context, Result};
use gitglass_core::{config::DefaultCommand, init_inline_terminal, init_terminal, prepare_signals};

const USAGE: &str = "\
Usage: gitglass [command] [args]

Commands:
  status [--inline | --fullscreen] Show Git status
  diff <file>       Interactively stage/unstage content changes in a file
  log [--inline | --fullscreen]    Interactive Git log viewer with branch filtering
  branch [--inline | --fullscreen] List and switch branches with configurable keybindings
  help              Show this help message

With no command, runs the configured default_command, or shows help.
--inline / --fullscreen alone apply to the configured default_command.
";

fn inline_override(command: DefaultCommand, args: &[OsString]) -> Result<Option<bool>> {
    match args {
        [] => Ok(None),
        [flag] if flag == "--inline" => Ok(Some(true)),
        [flag] if flag == "--fullscreen" => Ok(Some(false)),
        _ => bail!(
            "Usage: gitglass {} [--inline | --fullscreen] (got {args:?})",
            command.as_str()
        ),
    }
}

fn inline_mode(command: DefaultCommand, args: &[OsString]) -> Result<bool> {
    match inline_override(command, args)? {
        Some(inline) => Ok(inline),
        None => gitglass_core::config::load_inline_default(command)
            .context("Cannot load gitglass configuration"),
    }
}

fn run() -> Result<()> {
    let mut args: Vec<OsString> = env::args_os().skip(1).collect();

    if args.is_empty()
        || args
            .first()
            .is_some_and(|arg| arg == "--inline" || arg == "--fullscreen")
    {
        let default = gitglass_core::config::load_default_command()
            .context("Cannot load gitglass configuration")?;
        if let Some(command) = default {
            args.insert(0, OsString::from(command.as_str()));
        } else if !args.is_empty() {
            bail!("No default_command configured; specify status, log, or branch before the mode flag");
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
            let inline = inline_mode(DefaultCommand::Status, &args)?;

            // Load config *before* terminal init so parse errors print normally.
            let bindings =
                gitglass_core::config::load().context("Cannot load gitglass configuration")?;

            prepare_signals().context("Cannot prepare signal handlers")?;

            let (mut terminal, _guard) = if inline {
                init_inline_terminal(1)
            } else {
                init_terminal()
            }
            .context("Cannot initialise terminal")?;

            let run = if inline {
                gitglass_status::run_inline
            } else {
                gitglass_status::run
            };
            run(&mut terminal, &bindings).context("Status TUI exited with an error")?;

            Ok(())
        }
        Some("branch") => {
            let inline = inline_mode(DefaultCommand::Branch, &args)?;
            let bindings = gitglass_core::config::load_branch_bindings()
                .context("Cannot load gitglass configuration")?;
            prepare_signals().context("Cannot prepare signal handlers")?;
            let (mut terminal, _guard) = if inline {
                init_inline_terminal(1)
            } else {
                init_terminal()
            }
            .context("Cannot initialise terminal")?;
            let run = if inline {
                gitglass_branch::run_inline
            } else {
                gitglass_branch::run
            };
            run(&mut terminal, &bindings).context("Branch TUI exited with an error")?;
            Ok(())
        }
        Some("diff") => {
            if args.len() != 1 {
                bail!("gitglass diff requires exactly one file argument (got {args:?})");
            }

            let config = gitglass_core::config::load_diff_config()
                .context("Cannot load gitglass configuration")?;
            let theme = gitglass_diff::theme::Theme {
                removed_color: config.removed_color,
                line_number_color: config.line_number_color,
                ..Default::default()
            };
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

            gitglass_diff::run(&mut terminal, repo, snapshot, &file_name, theme)
                .context("Diff TUI exited with an error")?;

            Ok(())
        }
        Some("log") => {
            let inline = inline_mode(DefaultCommand::Log, &args)?;

            let run = if inline {
                gitglass_log::run_inline
            } else {
                gitglass_log::run_fullscreen
            };
            run().map_err(|e| anyhow::anyhow!("Log TUI exited with an error: {e:#}"))?;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presentation_flags_override_config_and_conflicts_are_rejected() {
        for command in [
            DefaultCommand::Status,
            DefaultCommand::Log,
            DefaultCommand::Branch,
        ] {
            assert_eq!(inline_override(command, &[]).unwrap(), None);
            for (flag, expected) in [("--inline", true), ("--fullscreen", false)] {
                assert_eq!(
                    inline_override(command, &[flag.into()]).unwrap(),
                    Some(expected)
                );
            }
            for flags in [
                ["--inline", "--fullscreen"],
                ["--fullscreen", "--fullscreen"],
                ["--inline", "--inline"],
            ] {
                let args = flags.map(OsString::from);
                assert!(inline_override(command, &args).is_err());
            }
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
