# Architecture

rustow has one planner and one executor. Every command that changes files builds a plan and runs it.

## Flow

1. `cli` parses arguments. `app` dispatches and prepares the context.
2. `repo` finds the repository and builds the `Context` (and the internal directories).
3. `config` loads `.rustow.toml`. `ignore` loads `.rustowignore`.
4. `source` discovers files and classifies each into a `Mode`. A file or configuration entry that cannot deploy goes to `Discovery::skipped`, with the reason. It also finds collisions and applies the selector.
5. `plan` decides an action for each destination, with the decision table. It integrates `fold`. It also plans removals.
6. `execute` predicts and executes the plan, or removes. `fsops` does the file operations.
7. `state` records each successful item. `report` prints the result.

## Modules

| Module | Role |
| --- | --- |
| `cli` | Argument definitions (clap). |
| `app` | Dispatch and preparation. |
| `repo` | `Context`, repository discovery, internal directories. |
| `config` (+ `edit`) | Schema, validation, comment-preserving edits. |
| `ignore` | Ignore rules and built-in exclusions. |
| `source` | Discover, classify, `Mode`, `Selector`, collisions. |
| `plan` | Decision table, build, fold integration, removal. |
| `fold` | Directory folding and unfolding. |
| `execute` | Predict, execute, remove. |
| `fsops` | Safe file operations (temp file, rename, ancestors). |
| `state` | Per-repository JSON state and lock. |
| `cache` | Rendered and decrypted entries, edit workspaces. |
| `template` | MiniJinja context and rendering. |
| `crypto` (age, gpg) | Encryption backends. |
| `enc` | `enc` and `enc edit`. |
| `init` | `init`. |
| `files` | `file add/remove/list`. |
| `inspect` | `diff`, `status`, `check`. |
| `doctor` | `doctor`. |
| `diff` | Diff output. |
| `report` | Output and JSON. |
| `hash` | Content hashes. |
| `paths` | Path types. |
| `error` | Errors and exit codes. |

## Modes

Link, FileCopy, DecryptLink, DecryptCopy, TemplateLink, TemplateCopy. The labels are link, copy, decrypt+link, decrypt+copy, template+link, template+copy. State and selectors use these labels. Output uses `source::shown_label`, which adds the backend: `decrypt(age)+link`, `decrypt(gpg)+template+link`. Commands choose modes: `link` takes Link only, `file apply` takes FileCopy and DecryptCopy, `template apply` takes templates, `apply` takes all.

`source::classify` applies one scope rule after the mode is selected (`scoped_mode`). Under `.rustowroot/` (`Scope::Root`), no source is a link:

| Selected mode | Home | Root |
| --- | --- | --- |
| Link | Link | skipped: register it with `rustow file add` |
| DecryptLink | DecryptLink | skipped: register it with `rustow file add` |
| TemplateLink | TemplateLink | TemplateCopy with empty metadata |
| FileCopy, DecryptCopy, TemplateCopy | unchanged | unchanged |

`classify` returns the reason as its error. `discover` puts the file in `Discovery::skipped`, and the commands print a warning for it (`report::Printer::warn`). `file add` discovers with a copy of the configuration in which each argument is registered. So `classify` decides with the same rule whether the file can deploy as a copy.

## Cache

Default `<repo>/.rustowcache`. Layout: `<cache>/rendered|decrypted/<repo-id>/<scope-id>/<dest>`. Directories are 0700 and files 0600. Before it writes, rustow creates the cache root, or sets its mode to 0700 without a message. It refuses a cache root that is not a directory (it never follows a symlink there) or that another user owns (`fsops::ensure_private_dir`). Entries that feed copies are refreshed.

## State

`$XDG_STATE_HOME/rustow`, else `~/.local/state/rustow` (or `defaults.state_dir`). The file `<repo-id>.json` (mode 0600, directory 0700) is keyed by absolute destination. It holds content hashes, not plaintext. The hashes are sensitive metadata. The file `<repo-id>.lock` stops concurrent runs (exit 4, "another rustow process"). rustow refuses corrupt, foreign or newer state. Move such a file away to continue. You must own the state directory.

## Tests

Integration tests are in `tests/scenario_*.rs`. They use the `Fixture` in `tests/common/mod.rs`. `tests/examples.rs` checks the example repository.
