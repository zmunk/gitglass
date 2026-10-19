use std::{ffi::OsString, io, path::PathBuf};

use ratatui::text::Text;

pub struct Status {
    pub text: Text<'static>,
    pub paths: Vec<PathBuf>,
}

impl Status {
    pub fn message(message: &'static str) -> Self {
        Self {
            text: Text::raw(message),
            paths: Vec::new(),
        }
    }
}

#[cfg(unix)]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(OsString::from_vec(bytes.to_vec()))
}

#[cfg(not(unix))]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(OsString::from(String::from_utf8_lossy(bytes).into_owned()))
}

// Keep each path on one display row, and never interpret path bytes as ANSI codes.
fn display_path(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

// `git status --short -z` preserves Git's configured colors, but uses raw,
// NUL-delimited paths (destination first for renames/copies).
pub fn parse(output: &[u8]) -> io::Result<Status> {
    if output.is_empty() {
        return Ok(Status::message("Working tree clean"));
    }
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Malformed NUL-delimited Git status",
        )
    };
    if output.last() != Some(&0) {
        return Err(invalid());
    }
    let mut records = output[..output.len() - 1].split(|byte| *byte == 0);
    let mut rendered = Vec::new();
    let mut paths = Vec::new();
    while let Some(record) = records.next() {
        let mut cursor = 0;
        let mut columns = Vec::new();
        while columns.len() < 3 {
            match record.get(cursor) {
                Some(0x1b) if record.get(cursor + 1) == Some(&b'[') => {
                    cursor += 2;
                    // Git emits SGR color sequences only in the status prefix.
                    while record
                        .get(cursor)
                        .is_some_and(|b| b.is_ascii_digit() || *b == b';' || *b == b':')
                    {
                        cursor += 1;
                    }
                    if record.get(cursor) != Some(&b'm') {
                        return Err(invalid());
                    }
                    cursor += 1;
                }
                Some(byte) => {
                    columns.push(*byte);
                    cursor += 1;
                }
                None => return Err(invalid()),
            }
        }
        if columns[2] != b' ' || cursor == record.len() {
            return Err(invalid());
        }
        let destination = &record[cursor..];
        paths.push(path_from_bytes(destination));
        rendered.extend_from_slice(&record[..cursor]);
        if columns[..2].iter().any(|c| matches!(c, b'R' | b'C')) {
            let source = records
                .next()
                .filter(|source| !source.is_empty())
                .ok_or_else(invalid)?;
            rendered.extend_from_slice(display_path(source).as_bytes());
            rendered.extend_from_slice(b" -> ");
        }
        rendered.extend_from_slice(display_path(destination).as_bytes());
        rendered.push(b'\n');
    }
    rendered.pop(); // No trailing empty display row.
    let text = super::status_text(&rendered)?;
    if text.lines.len() != paths.len() {
        return Err(invalid());
    }
    Ok(Status { text, paths })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colored_status_paths_are_separate_from_display() {
        let status = parse(b"\x1b[32mM\x1b[m  a file\0?? line\nbreak\x1b[31m\0").unwrap();
        assert_eq!(
            status.paths,
            vec![
                path_from_bytes(b"a file"),
                path_from_bytes(b"line\nbreak\x1b[31m")
            ]
        );
        assert_eq!(status.text.lines.len(), 2);
        assert_eq!(
            status.text.lines[0].spans[0].style.fg,
            Some(ratatui::style::Color::Green)
        );
        let displayed = status.text.lines[1]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(displayed.contains(r"line\nbreak\u{1b}[31m"));
    }

    #[test]
    fn renamed_and_deleted_entries_supply_destination_paths() {
        let status = parse(b"R  new name\0old name\0 D missing\0").unwrap();
        assert_eq!(
            status.paths,
            vec![PathBuf::from("new name"), PathBuf::from("missing")]
        );
        assert_eq!(status.text.lines[0].to_string(), "R  old name -> new name");
    }

    #[test]
    fn informational_rows_have_no_paths() {
        assert!(parse(b"").unwrap().paths.is_empty());
        assert!(Status::message("Not a git repository").paths.is_empty());
    }

    #[test]
    fn rejects_truncated_records() {
        for bytes in [b"?? file".as_slice(), b"R  new\0", b"M\0", b"\0"] {
            assert!(parse(bytes).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn paths_preserve_non_utf8_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let status = parse(b"?? \xff\0").unwrap();
        assert_eq!(status.paths[0].as_os_str().as_bytes(), b"\xff");
    }
}
