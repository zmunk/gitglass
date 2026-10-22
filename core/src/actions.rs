use std::{
    io::{self, Read, Write},
    path::Path,
    process::{Command, ExitStatus, Stdio},
    thread,
};

use crate::{restore_terminal, suspend_terminal};
use ratatui::{backend::CrosstermBackend, Terminal};

use crate::config::Binding;

fn command(binding: &Binding, file: Option<&Path>, cwd: &Path) -> Command {
    let mut command = Command::new("sh");
    command.arg("-c").arg(&binding.command).current_dir(cwd);
    match file {
        Some(file) => {
            command.env("FILE", file);
        }
        None => {
            command.env_remove("FILE");
        }
    }
    command
}

fn hash_command(binding: &Binding, hash: Option<&str>, cwd: &Path) -> Command {
    let mut command = command(binding, None, cwd);
    match hash {
        Some(hash) => {
            command.env("HASH", hash);
        }
        None => {
            command.env_remove("HASH");
        }
    }
    command
}

// Keep stdin/stdout attached to the terminal and forward stderr while capturing
// it for failure feedback. Drain concurrently so verbose commands cannot block.
fn run_command(mut command: Command) -> io::Result<(ExitStatus, Vec<u8>)> {
    let mut child = command.stderr(Stdio::piped()).spawn()?;
    let mut stderr = child.stderr.take().expect("stderr is piped");
    let reader = thread::spawn(move || {
        let mut captured = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            let count = match stderr.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            captured.extend_from_slice(&buffer[..count]);
            // Still drain the pipe if the terminal can no longer be written.
            let _ = io::stderr().write_all(&buffer[..count]);
        }
        Ok(captured)
    });
    let status = child.wait();
    let stderr = reader
        .join()
        .map_err(|_| io::Error::other("stderr reader panicked"))?;
    Ok((status?, stderr?))
}

fn result_message(binding: &Binding, status: ExitStatus, stderr: &[u8]) -> Option<String> {
    if status.success() {
        return binding.success_message.clone();
    }
    Some(
        match status
            .code()
            .and_then(|code| binding.exit_messages.get(&code))
        {
            Some(message) => message.clone(),
            None => {
                let stderr = String::from_utf8_lossy(stderr);
                let mut lines = stderr.lines();
                match lines.next() {
                    Some(first) => {
                        let remaining = lines.count();
                        if remaining == 0 {
                            first.to_owned()
                        } else {
                            format!("{first} [{remaining} more lines]")
                        }
                    }
                    None => format!("{}: command failed ({status})", binding.label),
                }
            }
        },
    )
}

pub fn execute(
    terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
    binding: &Binding,
    file: Option<&Path>,
    cwd: &Path,
) -> io::Result<Option<String>> {
    if binding.requires_file && file.is_none() {
        return Ok(Some(format!(
            "{}: This action requires a focused file",
            binding.label
        )));
    }
    execute_command(terminal, binding, command(binding, file, cwd))
}

/// Execute a log action, exporting the selected commit as shell data.
pub fn execute_hash<W: Write>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    binding: &Binding,
    hash: Option<&str>,
    cwd: &Path,
) -> io::Result<Option<String>> {
    if hash.is_none() && crate::config::requires_variable(&binding.command, "HASH") {
        return Ok(Some(format!(
            "{}: This action requires a focused commit",
            binding.label
        )));
    }
    execute_command(terminal, binding, hash_command(binding, hash, cwd))
}

fn execute_command<W: Write>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    binding: &Binding,
    command: Command,
) -> io::Result<Option<String>> {
    if binding.suspend {
        suspend_terminal(terminal, binding.leave_alternate_screen)?;
        let result = run_command(command);
        restore_terminal(terminal)?;
        Ok(match result {
            Ok((status, stderr)) => result_message(binding, status, &stderr),
            Err(error) => Some(format!(
                "{}: Could not launch command: {error}",
                binding.label
            )),
        })
    } else {
        let mut command = command;
        let output = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();
        Ok(match output {
            Ok(output) => {
                let combined: Vec<u8> = output.stdout.into_iter().chain(output.stderr).collect();
                result_message(binding, output.status, &combined)
            }
            Err(error) => Some(format!(
                "{}: Could not launch command: {error}",
                binding.label
            )),
        })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::config::{parse_key, requires_file};
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn commit_hash_is_environment_data_not_shell_source() {
        let hash = "abc1234; printf injected";
        let output = hash_command(
            &binding("printf '%s' \"$HASH\""),
            Some(hash),
            Path::new("/"),
        )
        .output()
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, hash.as_bytes());
    }

    #[test]
    fn missing_commit_removes_inherited_hash() {
        let command = hash_command(&binding("true"), None, Path::new("/"));
        assert!(command
            .get_envs()
            .any(|(key, value)| key == "HASH" && value.is_none()));
    }

    fn binding(source: &str) -> Binding {
        Binding {
            key: parse_key("e").unwrap(),
            command: source.to_owned(),
            label: "test".into(),
            help_text: "test".into(),
            requires_file: requires_file(source),
            exit_messages: Default::default(),
            success_message: None,
            suspend: true,
            leave_alternate_screen: true,
        }
    }

    #[test]
    fn exit_messages_are_specific_to_binding_and_status() {
        use std::os::unix::process::ExitStatusExt;

        let mut custom = binding("exit 10");
        custom.exit_messages.insert(10, "Partially staged".into());
        assert_eq!(
            result_message(&custom, ExitStatus::from_raw(10 << 8), b"ignored").as_deref(),
            Some("Partially staged")
        );
        assert!(result_message(&custom, ExitStatus::from_raw(0), b"ignored").is_none());
        custom.success_message = Some("Done".into());
        assert_eq!(
            result_message(&custom, ExitStatus::from_raw(0), b"ignored").as_deref(),
            Some("Done")
        );
        assert_eq!(
            result_message(&custom, ExitStatus::from_raw(10 << 8), b"ignored").as_deref(),
            Some("Partially staged")
        );
        assert!(result_message(&binding("true"), ExitStatus::from_raw(0), b"").is_none());
        for (binding, status) in [
            (&custom, ExitStatus::from_raw(11 << 8)),
            (&custom, ExitStatus::from_raw(15)),
            (&binding("exit 10"), ExitStatus::from_raw(10 << 8)),
        ] {
            assert!(result_message(binding, status, b"")
                .unwrap()
                .contains("command failed"));
        }
    }

    #[test]
    fn failures_show_only_first_stderr_line_and_remaining_count() {
        use std::os::unix::process::ExitStatusExt;

        let binding = binding("exit 1");
        for (stderr, expected) in [
            (&b"problem"[..], "problem"),
            (&b"problem\n"[..], "problem"),
            (&b"problem\ncontext\n"[..], "problem [1 more lines]"),
            (&b"problem\r\ncontext\r\nlast"[..], "problem [2 more lines]"),
            (&b"problem\n\n"[..], "problem [1 more lines]"),
            (&b"bad \xff\n"[..], "bad \u{fffd}"),
        ] {
            assert_eq!(
                result_message(&binding, ExitStatus::from_raw(1 << 8), stderr).as_deref(),
                Some(expected)
            );
        }
    }

    #[test]
    fn running_commands_captures_stderr_and_exit_status() {
        let (status, stderr) = run_command(command(
            &binding("printf 'problem\\ncontext\\n' >&2; exit 7"),
            None,
            Path::new("/"),
        ))
        .unwrap();
        assert_eq!(status.code(), Some(7));
        assert_eq!(stderr, b"problem\ncontext\n");
    }

    #[test]
    fn filenames_are_environment_data_not_shell_source() {
        let file = Path::new("spaces ' \" ; $(false)\n--file");
        let output = command(
            &binding("printf '%s' \"$FILE\""),
            Some(file),
            Path::new("/"),
        )
        .output()
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, file.as_os_str().as_bytes());
    }

    #[test]
    fn non_utf8_filename_is_preserved() {
        use std::ffi::OsStr;
        let file = Path::new(OsStr::from_bytes(b"non-utf8-\xff"));
        let output = command(
            &binding("printf '%s' \"$FILE\""),
            Some(file),
            Path::new("/"),
        )
        .output()
        .unwrap();
        assert_eq!(output.stdout, file.as_os_str().as_bytes());
    }

    #[test]
    fn no_file_explicitly_removes_inherited_variable() {
        let command = command(&binding("test -z \"${FILE+x}\""), None, Path::new("/"));
        assert!(command
            .get_envs()
            .any(|(key, value)| key == "FILE" && value.is_none()));
        assert!(command
            .get_current_dir()
            .is_some_and(|cwd| cwd == Path::new("/")));
    }

    #[test]
    fn commands_run_in_requested_directory_and_report_failure() {
        let output = command(&binding("pwd"), None, Path::new("/"))
            .output()
            .unwrap();
        assert_eq!(output.stdout, b"/\n");
        let status = command(&binding("exit 7"), None, Path::new("/"))
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(7));
    }
}
