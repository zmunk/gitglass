use std::{collections::HashMap, fs, io, path::PathBuf};

use crossterm::event::{KeyCode, KeyModifiers};
use serde::Deserialize;

pub type Key = (KeyCode, KeyModifiers);

pub const BUILTINS: &[(&str, &str)] = &[
    ("q", "quit"),
    ("j", "move down"),
    ("k", "move up"),
    ("ctrl-d", "half page down"),
    ("ctrl-u", "half page up"),
    ("?", "toggle keybinding help"),
];

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBinding {
    key: String,
    command: String,
    description: Option<String>,
    success_message: Option<String>,
    #[serde(default)]
    exit_messages: HashMap<String, String>,
    #[serde(default = "default_true")]
    suspend: bool,
    #[serde(default)]
    leave_alternate_screen: bool,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusSection {
    #[serde(default)]
    keybindings: Vec<RawBinding>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default)]
    status: StatusSection,
}

#[derive(Debug)]
pub struct Binding {
    pub key: Key,
    pub command: String,
    pub label: String,
    pub help_text: String,
    pub requires_file: bool,
    pub exit_messages: HashMap<i32, String>,
    pub success_message: Option<String>,
    pub suspend: bool,
    pub leave_alternate_screen: bool,
}

pub fn normalize_key(code: KeyCode, mut modifiers: KeyModifiers) -> Key {
    let code = match code {
        KeyCode::BackTab => {
            modifiers.insert(KeyModifiers::SHIFT);
            KeyCode::Tab
        }
        KeyCode::Char(c) if c.is_ascii_alphabetic() => {
            let c = if modifiers.contains(KeyModifiers::SHIFT) {
                c.to_ascii_uppercase()
            } else {
                c
            };
            modifiers.remove(KeyModifiers::SHIFT);
            KeyCode::Char(c)
        }
        other => other,
    };
    (code, modifiers)
}

pub fn parse_key(value: &str) -> Result<Key, String> {
    let mut rest = value;
    let mut modifiers = KeyModifiers::NONE;
    loop {
        let Some((prefix, suffix)) = rest.split_once('-') else {
            break;
        };
        let modifier = match prefix.to_ascii_lowercase().as_str() {
            "ctrl" => KeyModifiers::CONTROL,
            "alt" => KeyModifiers::ALT,
            "shift" => KeyModifiers::SHIFT,
            _ => break,
        };
        if modifiers.contains(modifier) {
            return Err(format!("Repeated modifier in key {value:?}"));
        }
        modifiers.insert(modifier);
        rest = suffix;
    }
    let mut chars = rest.chars();
    let first = chars.next();
    let code = if let (Some(c), None) = (first, chars.next()) {
        if c.is_control() || c.is_whitespace() {
            return Err(format!("Use a named key for {value:?}"));
        }
        KeyCode::Char(c)
    } else {
        match rest.to_ascii_lowercase().as_str() {
            "space" => KeyCode::Char(' '),
            "enter" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            "backtab" => KeyCode::BackTab,
            "backspace" => KeyCode::Backspace,
            "delete" => KeyCode::Delete,
            "insert" => KeyCode::Insert,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" => KeyCode::PageUp,
            "pagedown" => KeyCode::PageDown,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            name => match name.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                Some(n @ 1..=24) => KeyCode::F(n),
                _ => return Err(format!("Unsupported key {value:?}")),
            },
        }
    };
    Ok(normalize_key(code, modifiers))
}

pub fn requires_file(command: &str) -> bool {
    command.match_indices('$').any(|(index, _)| {
        let rest = &command[index + 1..];
        rest.starts_with("{FILE}")
            || rest.strip_prefix("FILE").is_some_and(|suffix| {
                !suffix.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
            })
    })
}

pub fn parse_config(contents: &str) -> io::Result<Vec<Binding>> {
    let config: Config = toml::from_str(contents)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let mut occupied = HashMap::new();
    for &(key, description) in BUILTINS {
        occupied.insert(parse_key(key).unwrap(), format!("built-in {description}"));
    }
    let mut bindings = Vec::new();
    let mut errors = Vec::new();
    for raw in config.status.keybindings {
        let label = match raw.description.as_deref() {
            Some(description) => format!("{} ({description})", raw.key),
            None => raw.key.clone(),
        };
        let key = match parse_key(&raw.key) {
            Ok(key) => key,
            Err(error) => {
                errors.push(error);
                continue;
            }
        };
        if let Some(previous) = occupied.get(&key) {
            errors.push(format!(
                "Key {:?}: {label} conflicts with {previous}",
                raw.key
            ));
        } else {
            occupied.insert(key, label.clone());
        }
        if raw.command.trim().is_empty() {
            errors.push(format!("{label}: command must not be empty"));
        }
        if raw
            .success_message
            .as_ref()
            .is_some_and(|message| message.trim().is_empty())
        {
            errors.push(format!("{label}: success message must not be empty"));
        }
        let mut exit_messages = HashMap::new();
        for (code, message) in raw.exit_messages {
            match code.parse::<i32>() {
                Ok(code) if (1..=255).contains(&code) => {
                    if message.trim().is_empty() {
                        errors.push(format!(
                            "{label}: exit message for {code} must not be empty"
                        ));
                    }
                    if exit_messages.insert(code, message).is_some() {
                        errors.push(format!("{label}: duplicate exit message for {code}"));
                    }
                }
                _ => errors.push(format!(
                    "{label}: invalid exit code {code:?}; expected 1–255"
                )),
            }
        }
        bindings.push(Binding {
            key,
            help_text: format!(
                "{}  {}",
                raw.key,
                raw.description.as_deref().unwrap_or(&raw.command)
            ),
            requires_file: requires_file(&raw.command),
            command: raw.command,
            exit_messages,
            success_message: raw.success_message,
            suspend: raw.suspend,
            leave_alternate_screen: raw.leave_alternate_screen,
            label,
        });
    }
    if errors.is_empty() {
        Ok(bindings)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            errors.join("\n"),
        ))
    }
}

/// Load keybindings from `$XDG_CONFIG_HOME/gitglass/config.toml` (falling
/// back to `~/.config/gitglass/config.toml`).  Returns an empty `Vec` when
/// the file does not exist.
pub fn load() -> io::Result<Vec<Binding>> {
    let root = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".config"))
        })
        .ok_or_else(|| {
            io::Error::other("Cannot locate global config: set HOME or XDG_CONFIG_HOME")
        })?;
    let path = root.join("gitglass/config.toml");
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(io::Error::new(
                error.kind(),
                format!("{}: {error}", path.display()),
            ))
        }
    };
    parse_config(&contents)
        .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_reference_is_lexical_and_has_a_boundary() {
        for command in [
            "echo $FILE",
            "echo ${FILE}",
            "'$FILE'",
            r"\$FILE",
            "$FILE/path",
        ] {
            assert!(requires_file(command), "{command}");
        }
        for command in [
            "git fetch",
            "$FILENAME",
            "$FILE_SUFFIX",
            "${FILE:-fallback}",
            "${FILENAME}",
        ] {
            assert!(!requires_file(command), "{command}");
        }
    }

    #[test]
    fn parses_commands_and_descriptions() {
        let bindings = parse_config(
            "[status]\n[[status.keybindings]]\nkey='e'\ncommand='nvim \"$FILE\"'\ndescription='Edit'",
        )
        .unwrap();
        assert_eq!(bindings[0].command, "nvim \"$FILE\"");
        assert!(bindings[0].requires_file);
        assert_eq!(bindings[0].label, "e (Edit)");
        assert!(parse_config("").unwrap().is_empty());
    }

    #[test]
    fn parses_per_binding_exit_messages() {
        let bindings = parse_config(
            "[status]\n[[status.keybindings]]\nkey='s'\ncommand='exit 10'\nexit_messages = { '10' = 'Partially staged', '11' = 'Fully staged' }\n[[status.keybindings]]\nkey='e'\ncommand='true'",
        ).unwrap();
        assert_eq!(bindings[0].exit_messages[&10], "Partially staged");
        assert_eq!(bindings[0].exit_messages[&11], "Fully staged");
        assert!(bindings[1].exit_messages.is_empty());
    }

    #[test]
    fn parses_optional_success_message() {
        let bindings = parse_config(
            "[status]\n[[status.keybindings]]\nkey='s'\ncommand='true'\nsuccess_message='Staged'\n[[status.keybindings]]\nkey='e'\ncommand='true'",
        )
        .unwrap();
        assert_eq!(bindings[0].success_message.as_deref(), Some("Staged"));
        assert!(bindings[1].success_message.is_none());
        for message in ["", "   "] {
            let config =
                format!("[status]\n[[status.keybindings]]\nkey='s'\ncommand='true'\nsuccess_message='{message}'");
            assert!(parse_config(&config).is_err());
        }
    }

    #[test]
    fn rejects_invalid_exit_messages() {
        for mapping in [
            "{ '0' = 'Success' }",
            "{ '-1' = 'Invalid' }",
            "{ '256' = 'Invalid' }",
            "{ 'oops' = 'Invalid' }",
            "{ '10' = '   ' }",
            "{ '10' = 'One', '010' = 'Two' }",
        ] {
            let config =
                format!("[status]\n[[status.keybindings]]\nkey='s'\ncommand='true'\nexit_messages = {mapping}");
            assert!(parse_config(&config).is_err(), "{mapping}");
        }
    }

    #[test]
    fn rejects_invalid_config_and_conflicts() {
        for contents in [
            "not toml",
            "[[status.keybindings]]\nkey='e'",
            "[[status.keybindings]]\nkey='e'\ncommand=' '",
            "[[status.keybindings]]\nkey='ctrl-nope'\ncommand='true'",
            "[[status.keybindings]]\nkey='q'\ncommand='true'",
            "[[status.keybindings]]\nkey='e'\ncommand='true'\n[[status.keybindings]]\nkey='e'\ncommand='false'",
            "[[status.keybindings]]\nkey='E'\ncommand='true'\n[[status.keybindings]]\nkey='shift-e'\ncommand='false'",
        ] {
            assert!(parse_config(contents).is_err(), "{contents}");
        }
        let error = parse_config(
            "[[status.keybindings]]\nkey='q'\ncommand='true'\ndescription='Custom quit'",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("Custom quit") && error.contains("built-in quit"));
    }

    #[test]
    fn all_help_and_navigation_keys_are_reserved() {
        for &(key, description) in BUILTINS {
            let error = parse_config(&format!(
                "[[status.keybindings]]\nkey='{key}'\ncommand='true'"
            ))
            .unwrap_err()
            .to_string();
            assert!(
                error.contains(&format!("built-in {description}")),
                "{error}"
            );
        }
    }

    #[test]
    fn page_keys_are_available_for_custom_commands() {
        let bindings = parse_config(
            "[[status.keybindings]]\nkey='pageup'\ncommand='true'\n[[status.keybindings]]\nkey='pagedown'\ncommand='true'",
        )
        .unwrap();
        assert_eq!(bindings.len(), 2);
    }

    #[test]
    fn keys_normalize_consistently() {
        assert_eq!(parse_key("shift-e"), parse_key("E"));
        assert_eq!(parse_key("backtab"), parse_key("shift-tab"));
        assert_ne!(parse_key("ctrl-j"), parse_key("j"));
        assert_eq!(
            parse_key("ctrl--").unwrap(),
            (KeyCode::Char('-'), KeyModifiers::CONTROL)
        );
        assert!(parse_key("f24").is_ok());
        assert!(parse_key("f25").is_err());
    }
}
