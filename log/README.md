# gitglass-log

A Ratatui viewer for `git log --oneline --graph --decorate`. The selected
line is bold; Git colors and graph connector lines are preserved. Keyboard
hints appear in gray.

Commits on HEAD's first-parent history have display-only relative labels:
`HEAD^` and `HEAD~2` only. All immediate parents of HEAD are labeled;
when HEAD is a merge, its other parents use `HEAD^2`, `HEAD^3`, etc. These follow Git revision
semantics (not visible row numbers), appear alongside existing decorations,
and refresh with the log. They do not create Git tags.

## Run

With Rust installed, run `cargo run --release` from this project inside a Git
repository. Alternatively, install with `cargo install --path .`, then run
`gitglass-log` inside any Git repository.  This crate is also integrated as
the `gitglass log` subcommand.

## Keys

- `j` / `k` (or arrow keys): move down / up one line
- `Ctrl-d` / `Ctrl-u`: move down / up half the visible log area
- `/` in the log: enter live search input (or edit the existing search)
- `Enter` in search input: select the first match and enter search result mode
- `Left` / `Right` in search input: move the visible text cursor
- `Home` / `End` in search input: move to the start / end of the query
- `Backspace` / `Delete` in search input: delete before / at the cursor
- `Ctrl-u` in search input: clear the query
- `n` / `N` in search results: select the next / previous occurrence, without wrapping
- `Esc` in either search mode: clear search highlights and return to normal mode, keeping the cursor position
- `b`: toggle the right-hand branch list (local and remote branches); focus it when opening
- `Space` in the branch list: toggle inclusion in the log (`[x]` means included)
- `a` in the branch list: select all branches (including hidden matches), or clear all if all are selected
- `/` in the branch list: edit a live, case-insensitive substring filter shown beside the title
- `Enter` while filtering: return focus to the filtered list, keeping the query
- `Backspace` while filtering: delete the last character
- `Esc`: clear an active filter first; otherwise close the branch list and return to the log; quit if already in the log
- `q` or `Ctrl-c`: quit

Search is a case-sensitive literal substring search across the displayed log,
including hashes, decorations, and subjects. All occurrences are highlighted in
yellow; the active result is cyan. While typing, the first match is scrolled into
view without moving the cursor. Enter selects it; `n` / `N` select subsequent
occurrences (including multiple occurrences on one line) and scroll as needed.
Moving the log cursor off the active result returns it to yellow; `n` / `N`
then search strictly after / before the cursor's current line.
The footer reports missing results and navigation boundaries. `/` from results
retains the query; Esc cancels it. Ordinary shortcuts are text while typing
(except Ctrl-c, which still quits).

The viewport follows the selection and adapts to terminal resizing.
The branch list is sorted by committer date, newest first. It uses the same
navigation keys and displays the current branch in green with a `*` marker;
the cursor remains bold. Closing it preserves your log position; opening it
refreshes the branch list. Filtering only changes which branches are visible;
it does not change their inclusion in the log. `Space` toggles the highlighted
visible branch, while `a` still affects all branches.
The current branch is included (`[x]`) by default. The log shows commits reachable
from the included branches, without duplicates. With no branches included, the
log is empty and displays a prompt to select branches. Detached HEAD starts with
no branches selected. Inclusion persists when the menu is closed and reopened. Toggling refreshes the log and resets its cursor to the top;
if loading fails, the previous log and inclusion remain unchanged.
This only filters history—it does not check branches out.
Git log errors are reported before entering the terminal UI; branch-loading errors
appear in the branch pane.

## Custom keybindings

Log bindings use the same global config and command options as status:
`$XDG_CONFIG_HOME/gitglass/config.toml` (or `~/.config/gitglass/config.toml`).
For example:

```toml
[[log.keybindings]]
key = "y"
command = 'copy "$HASH"'
description = "Copy commit hash"
success_message = "Commit hash copied"
```

Commands run through `sh -c` with the selected commit's **full hash** exported
as `$HASH`, not substituted into shell source. `copy` must be available to
that shell (interactive shell aliases/functions are not automatically loaded).
There is no built-in clipboard command or default `y` binding.
Configured bindings appear in the log footer using their descriptions (or
commands when no description is set).
Bindings apply only while the log has focus, not while entering a log search
or navigating or filtering branches. Commands referencing `$HASH` or `${HASH}` require a selected commit;
graph-only lines and empty logs do not provide one.
Built-in log keys cannot be overridden, and duplicate bindings are rejected.

As in status, `suspend` defaults to `true`; set `suspend = false` to capture
command output without suspending the UI. `leave_alternate_screen`,
`success_message`, and `exit_messages` work the same way as status. Command
feedback appears in the footer until the next keypress in the log.

## Development

Run `cargo test` for navigation, scrolling, and bold-selection rendering tests.
