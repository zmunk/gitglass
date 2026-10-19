# gitglass

A Git porcelain TUI combining status and partial-content staging in a single
binary.

## Usage

```bash
gitglass status              # Interactive git status with configurable keybindings
gitglass diff <file>         # Stage/unstage content changes line-by-line or by hunk
```

## Installation

```bash
cargo install --path .
```

Requires a recent Rust toolchain and Git CLI (2.31+).

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
├── diff/           ← gitglass-diff library (partial staging TUI)
└── src/main.rs     ← subcommand dispatch
```

## License

MIT
