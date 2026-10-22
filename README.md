# gitglass

A Git porcelain TUI combining status and partial-content staging in a single
binary.

## Usage

```bash
gitglass status              # Interactive git status with configurable keybindings
gitglass branch              # List and switch branches
gitglass diff <file>         # Stage/unstage content changes line-by-line or by hunk
```

## Installation

```bash
cargo install --path .
```

Requires a recent Rust toolchain and Git CLI (2.31+).

## Default command

To run `gitglass status` when invoking `gitglass` without arguments, add this
at the top of `$XDG_CONFIG_HOME/gitglass/config.toml` (or
`~/.config/gitglass/config.toml`), before any table headers:

```toml
default_command = "status"
```

Supported defaults are `status`, `log`, and `branch`. Without this setting, `gitglass`
shows help. Explicit subcommands and `--help` / `-h` always take precedence.

## Subcommands

### `gitglass status`

Displays `git status -s` output with Git's configured colours.  Navigate with
`j`/`k`, scroll with `Ctrl-D`/`Ctrl-U`, and press `?` to toggle the keybinding
help.

Configure custom keybindings in `$XDG_CONFIG_HOME/gitglass/config.toml`
(falling back to `~/.config/gitglass/config.toml`):

```toml
[status]
[[status.keybindings]]
key = "e"
command = 'nvim -- "$FILE"'
description = "Edit focused file"

[[status.keybindings]]
key = "s"
command = 'git add -- "$FILE"'
description = "Stage focused file"

[[status.keybindings]]
key = "g"
command = 'lazygit'
description = "Open lazygit"
```

Keys support `ctrl-`, `alt-`, and `shift-` modifiers.  Commands run through
`sh -c` with the focused file path exported as `$FILE`.  See the expanded
help (`?`) for the full key reference.

Log commands support the same options under `[[log.keybindings]]`, with the
selected commit's full hash exported as `$HASH`:

```toml
[[log.keybindings]]
key = "y"
command = 'copy "$HASH"'
description = "Copy commit hash"
success_message = "Commit hash copied"
```

The `copy` command must be available to `sh -c`. See [log documentation](log/README.md)
for selection behavior and command options.

### `gitglass branch`

Lists local branches first, followed by grey remote branches. Each group uses
the log branch menu's most-recent-commit-first order. The current branch is
green and marked `*`; the cursor is
marked `>`. Navigate with `j`/`k` or arrows, scroll with `Ctrl-D`/`Ctrl-U`,
toggle help with `?`, and quit with `q` or `Esc`.

Press Space to switch to the selected branch. Switching is refused if there
are staged, unstaged, or untracked changes (including dirty submodules).
Remote branches remain selectable for custom actions, but Space does nothing
when a remote branch is selected. Git failures appear in the footer without closing
the list.

Custom actions use the same options as status/log under `[[branch.keybindings]]`.
The selected branch's name is exported as `$BRANCH` (e.g. `topic` or
`origin/topic`, without the `refs/heads/` or `refs/remotes/` prefix):

```toml
[[branch.keybindings]]
key = "y"
command = 'copy "$BRANCH"'
description = "Copy branch name"
success_message = "Branch name copied"
suspend = false
```

Quote `"$BRANCH"` in commands to keep branch names as shell data. The list
refreshes periodically and after actions, preserving the selected branch.

### `gitglass diff <file>`

Open a single tracked file for partial-content staging.  Three tabs are
available:

| Tab | Compares | Actions |
| --- | --- | --- |
| **Working** | Index ↔ working tree | Stage, discard |
| **Staged** | HEAD ↔ index | Unstage |
| **Preview** | Index content | Read-only |

Navigate changes with `j`/`k`, toggle line/hunk mode with `Tab`, and use
`v` for anchored multi-line selection.  Changes apply immediately — there is
no commit step.  See the on-screen footer for the full control reference.

## Workspace structure

```
gitglass/           ← workspace root & main binary
├── core/           ← shared terminal init, signal handling
├── status/         ← gitglass-status library (git status TUI)
├── branch/         ← gitglass-branch library (branch switching TUI)
├── diff/           ← gitglass-diff library (partial staging TUI)
└── src/main.rs     ← subcommand dispatch
```

## License

MIT
