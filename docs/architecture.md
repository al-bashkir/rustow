# Architecture

rustow has one planner and one executor. Every command that changes files builds a plan and runs it.

## Flow

1. `cli` parses arguments. `app` dispatches and prepares the context.
2. `repo` finds the repository and builds the `Context` (and the internal directories).
3. `config` loads `rustow.toml`. `ignore` loads `.rustowignore`.
4. `source` discovers files and classifies each into a `Mode`. A file or configuration entry that cannot deploy goes to `Discovery::skipped`, with the reason. It also finds collisions and applies the selector.
5. `plan` decides an action for each destination, with the decision table. It integrates `fold`. It also plans removals.
6. `execute` predicts and executes the plan, or removes. `fsops` does the file operations. A system file that the user cannot write (`Deploy::sudo`, `Removal::sudo`) waits until the end of the run. Then `sudo::call` sends all of them to one `sudo rustow __sudo-helper`. The helper runs `execute::write_dest` or `execute::remove_dest`, the same code as a normal run, as root.
7. `execute` records each successful item in the `State` in memory. `app::finish` saves the state file once, at the end of the run. `report` prints the result.

## Modules

| Module              | Role                                                   |
| ------------------- | ------------------------------------------------------ |
| `cli`               | Argument definitions (clap).                           |
| `complete`          | Shell completion of repository paths.                  |
| `app`               | Dispatch and preparation.                              |
| `repo`              | `Context`, repository discovery, internal directories. |
| `config` (+ `edit`) | Schema, validation, comment-preserving edits.          |
| `ignore`            | Ignore rules and built-in exclusions.                  |
| `source`            | Discover, classify, `Mode`, `Selector`, collisions.    |
| `plan`              | Decision table, build, fold integration, removal.      |
| `fold`              | Directory folding and unfolding.                       |
| `execute`           | Predict, execute, remove.                              |
| `fsops`             | Safe file operations (temp file, rename, ancestors).   |
| `state`             | Per-repository JSON state and lock.                    |
| `transfer`          | `state export/import` of the registrations.            |
| `sudo`              | The sudo helper: client (`call`) and helper (`serve`). |
| `cache`             | Rendered and decrypted entries, edit workspaces.       |
| `template`          | MiniJinja context and rendering.                       |
| `crypto` (age, gpg) | Encryption backends.                                   |
| `enc`               | `enc` and `enc edit`.                                  |
| `init`              | `init`.                                                |
| `files`             | `file add/remove/list`.                                |
| `inspect`           | `diff`, `status`, `check`.                             |
| `doctor`            | `doctor`.                                              |
| `diff`              | Diff output.                                           |
| `report`            | Output and JSON.                                       |
| `hash`              | Content hashes.                                        |
| `paths`             | Path types.                                            |
| `error`             | Errors and exit codes.                                 |

## Modes

Link, FileCopy, DecryptLink, DecryptCopy, TemplateLink, TemplateCopy. The labels are link, copy, decrypt+link, decrypt+copy, template+link, template+copy. State and selectors use these labels. Output uses `source::shown_label`, which adds the backend: `decrypt(age)+link`, `decrypt(gpg)+template+link`. Commands choose modes: `link` takes Link only, `file apply` takes FileCopy and DecryptCopy, `template apply` takes templates, `apply` takes all.

`source::classify` applies one scope rule after the mode is selected (`scoped_mode`). Under `rustowroot/` (`Scope::Root`), no source is a link:

| Selected mode                       | Home         | Root                                        |
| ----------------------------------- | ------------ | ------------------------------------------- |
| Link                                | Link         | skipped: register it with `rustow file add` |
| DecryptLink                         | DecryptLink  | skipped: register it with `rustow file add` |
| TemplateLink                        | TemplateLink | TemplateCopy with empty metadata            |
| FileCopy, DecryptCopy, TemplateCopy | unchanged    | unchanged                                   |

With `defaults.mode = "copy"`, `scoped_mode` also makes a home TemplateLink a TemplateCopy with empty metadata. A home Link or DecryptLink stays a link source. `app::selected` decides if the command deploys it (`may_link`): only `rustow link <path>`, or a link registration in the state (`State::links`). `app::prepare` adds the registrations for `rustow link <path>`; `unlink` removes them.

`classify` returns the reason as its error. `discover` puts the file in `Discovery::skipped`, and the commands print a warning for it (`report::Printer::warn`). A source is registered for copy deployment when `[[files]]` (`Config::files`) or `file add` (`State::files`) lists it. `Context::load_state` copies `State::files` into `Config::added_files`, and `Config::registration` checks both. A copy mode carries the metadata of its `[[files]]` or `[[template_rules]]` entry (`Mode::metadata`). `file add` discovers with a copy of the configuration in which each argument is registered. So `classify` decides with the same rule whether the file can deploy as a copy. `state import` runs the same check (`files::check_copies`) on each imported copy, and the check of `rustow link <path>` (`app::check_link`) on each imported link.

A skipped entry has a kind (`source::SkipKind`): `Present` for a path in the repository that cannot deploy (also a path that rustow cannot read), `Missing` for a registration whose source does not exist. Only a `Present` entry counts as an existing source, so a recorded destination of a `Missing` one is ORPHANED.

## Input and output

- rustow reads `rustow.toml`, `.rustowignore`, the state file and `state import` input with `fsops::read_limited`: a regular file, opened with `O_NONBLOCK`, with a size limit.
- rustow starts `gpg`, the sudo program and the editor by the absolute path that `fsops::find_program` returns. The lookup skips relative `PATH` entries.
- Human output passes each path and message through `report::shown`, which escapes control characters. `Printer::path` does it for destinations. JSON output does not use it.

## Cache

Default `<repo>/.rustowcache`. Layout: `<cache>/rendered|decrypted/<repo-id>/<scope-id>/<dest>`. Directories are 0700 and files 0600. Before it writes, rustow creates the cache root, or sets its mode to 0700 without a message. It refuses a cache root that is not a directory (it never follows a symlink there) or that another user owns (`fsops::ensure_private_dir`). Entries that feed copies are refreshed.

## State

`$XDG_STATE_HOME/rustow`, else `~/.local/state/rustow` (or `defaults.state_dir`). The file `<repo-id>.json` (mode 0600, directory 0700) is keyed by absolute destination. It also holds the registrations of this machine: `files` (`file add`) and `links` (`link <path>` in copy mode). It holds content hashes, not plaintext. The hashes are sensitive metadata. The file `<repo-id>.lock` stops concurrent runs (exit 4, "another rustow process"). `state::lock` keeps a lock only when the path still names the locked file (device and inode); else it tries again, at most 3 times. When the lock file is removed or replaced during a run (`Lock::replaced`), `app::save` warns before it saves the state. rustow refuses corrupt, foreign or newer state. Move such a file away to continue. You must own the state directory.

The file has a `version` (`state::STATE_VERSION`, now 1). `State::load` reads each version from 1 up to it and runs the steps in `state::MIGRATIONS` that the file needs, in order. A newer version fails with "written by a newer rustow". After a migration, `save` first keeps the old bytes as `backups/<repo-id>/state-v<N>.json`. A new field with a default needs no new version: `files` and `links` came that way. An older rustow ignores such a field and drops it when it saves. Raise `STATE_VERSION` only for a change that needs a step, and add one step that turns the old JSON into the new JSON.

## Tests

Integration tests are in `tests/scenario_*.rs`. They use the `Fixture` in `tests/common/mod.rs`. `tests/examples.rs` checks the example repository.
