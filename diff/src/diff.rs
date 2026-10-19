use similar::{ChangeTag, TextDiff};
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Context,
    Add,
    Remove,
}
#[derive(Clone, Debug)]
pub struct Row {
    pub kind: Kind,
    pub text: String,
    pub old: usize,
    pub new: usize,
}
#[derive(Clone, Debug, Default)]
pub struct Diff {
    pub rows: Vec<Row>,
    pub hunks: Vec<Range<usize>>,
}
impl Diff {
    pub fn between(old: &str, new: &str) -> Self {
        let diff = TextDiff::from_lines(old, new);
        let (mut a, mut b) = (0, 0);
        let mut rows = Vec::new();
        for change in diff.iter_all_changes() {
            let kind = match change.tag() {
                ChangeTag::Equal => Kind::Context,
                ChangeTag::Insert => Kind::Add,
                ChangeTag::Delete => Kind::Remove,
            };
            rows.push(Row {
                kind,
                text: change.value().to_owned(),
                old: a,
                new: b,
            });
            if kind != Kind::Add {
                a += 1;
            }
            if kind != Kind::Remove {
                b += 1;
            }
        }
        // A hunk includes up to three unchanged lines on either side. Adjacent
        // expanded intervals merge, just as Git merges changes six lines apart.
        let mut hunks: Vec<Range<usize>> = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            if row.kind == Kind::Context {
                continue;
            }
            let range = i.saturating_sub(3)..(i + 4).min(rows.len());
            if let Some(last) = hunks.last_mut() {
                if range.start <= last.end {
                    last.end = last.end.max(range.end);
                    continue;
                }
            }
            hunks.push(range);
        }
        Self { rows, hunks }
    }
    pub fn actionable(&self, i: usize) -> bool {
        self.rows.get(i).is_some_and(|r| r.kind != Kind::Context)
    }
    pub fn first(&self) -> Option<usize> {
        self.rows.iter().position(|r| r.kind != Kind::Context)
    }
    pub fn hunk(&self, i: usize) -> Option<usize> {
        self.hunks.iter().position(|r| r.contains(&i))
    }
    /// Forward applies selected changes to the old side. Reverse removes them
    /// from the new side. Replacement rows deliberately remain independent.
    pub fn apply(&self, selected: &[usize], reverse: bool) -> String {
        let mut result = String::new();
        for (i, r) in self.rows.iter().enumerate() {
            let chosen = selected.binary_search(&i).is_ok();
            let keep = match r.kind {
                Kind::Context => true,
                Kind::Add => chosen != reverse,
                Kind::Remove => chosen == reverse,
            };
            if keep {
                result.push_str(&r.text);
            }
        }
        result
    }
    pub fn retain_cursor(&self, previous: &Self, cursor: Option<usize>) -> Option<usize> {
        let Some(old) = cursor.and_then(|i| previous.rows.get(i)) else {
            return self.first();
        };
        // Identity first, coordinates only as a tie breaker/fallback. Never keep
        // a screen row just because it still exists after inserting content.
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.kind != Kind::Context)
            .min_by_key(|(_, r)| {
                (
                    !(r.kind == old.kind && r.text == old.text),
                    r.old.abs_diff(old.old) + r.new.abs_diff(old.new),
                )
            })
            .map(|(i, _)| i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_replacements() {
        let d = Diff::between("old\n", "new\n");
        assert_eq!(d.apply(&[1], false), "old\nnew\n");
        assert_eq!(d.apply(&[0], false), "");
        assert_eq!(d.apply(&[0, 1], false), "new\n");
        assert_eq!(d.apply(&[0], true), "old\nnew\n");
        assert_eq!(d.apply(&[1], true), "");
    }
    #[test]
    fn bytes_and_full_context() {
        let d = Diff::between("a\r\nb\r\nend", "a\r\nc\r\nend");
        assert_eq!(d.apply(&[1, 2], false), "a\r\nc\r\nend");
        assert_eq!(d.rows.len(), 4);
        assert_eq!(Diff::between("", "").first(), None);
    }
    #[test]
    fn hunk_grouping() {
        let old = (0..30).map(|n| format!("{n}\n")).collect::<String>();
        let new = old.replace("2\n", "two\n").replace("25\n", "twenty-five\n");
        let d = Diff::between(&old, &new);
        assert!(d.hunks.len() >= 2);
        assert!(d.rows.iter().any(|r| r.text == "29\n"));
    }
}
