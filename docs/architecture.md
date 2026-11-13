# Architecture

rustow has one planner and one executor. Every command that changes files builds a plan and runs it.

## Flow

1. `cli` parses arguments. `app` dispatches and prepares the context.
2. `repo` finds the repository and builds the `Context` (and the internal directories).
3. `config` loads `rustow.toml`. `ignore` loads `.rustowignore`.
4. `source` discovers files and classifies each into a `Mode`. A file or configuration entry that cannot deploy goes to `Discovery::skipped`, with the reason. It also finds collisions and applies the selector.
5. `plan` decides an action for each destination, with the decision table. It integrates `fold`. It also plans removals.
6. `execute` predicts and executes the plan, or removes. `fsops` does the file operations. A system file that the user cannot write (`Deploy::sudo`, `Removal::sudo`) waits until the end of the run. Then `sudo::call` sends all of them to one `sudo rustow __sudo-helper`, in lock-step frames: after the helper's first line (`sudo::READY`, without it no frame goes out), for each job, a JSON header line (the user's uid, the content length, the job) and the raw content, then one JSON result line back before the next frame. The helper (`sudo::serve_on`) runs `execute::write_dest` or `execute::remove_dest`, the same code as a normal run, as root, with the trust rule of the user. It stops after a job whose result it cannot write. It first refuses a job with a path, a name or a mode out of bounds (`sudo::refused`). A destination that the user cannot read is `Observed::Unreadable` (metadata only); the plan compares it with the state and puts the recorded hash in `Deploy::expect` (`Verdict::Remove` for a removal). The recheck of the executor (`execute::recheck`) reads it, as root in the helper; other content is an `Error::Conflict`, which comes back as `Done::conflict`. A destination in a directory that the user cannot search is `Observed::Hidden` (nothing known): `plan::decide_hidden` creates it without a record, and else compares the desired hash and metadata with the record (`Recorded::Copy`); the recheck (`execute::found_hidden`) accepts only nothing there or the recorded content. On a conflict, `execute::update` still accepts a file with the desired content (`holds_desired`): a run whose client was killed wrote it and recorded nothing.
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
| `pending`           | Journal of temporary files; cleanup after a crash.     |
| `transfer`          | `state export/import/adopt`.                           |
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

With `defaults.mode = "copy"`, `scoped_mode` also makes a home TemplateLink a TemplateCopy with empty metadata. A home Link or DecryptLink stays a link source. `app::selected` decides if the command deploys it (`may_link`): only `rustow link <path>`, or a link registration in the state (`State::is_link`). `app::prepare` adds the registrations for `rustow link <path>`; `unlink` removes them.

`State::registrations` maps each registered source to one `state::Registration`: `Copy` (`file add`) or `Link` (copy-mode `link <path>`). So a source cannot have both. The state file keeps the JSON lists `files` and `links` (`state::Lists`); a source in both lists loads as a copy, and the next save writes it once. `file add` replaces a link registration. `rustow link <path>` of a registered copy is a usage error that names `rustow file remove` (`app::nothing_deployed`). `state import` follows the same rule.

`classify` returns the reason as its error. `discover` puts the file in `Discovery::skipped`, and the commands print a warning for it (`report::Printer::warn`). A source is registered for copy deployment when `[[files]]` (`Config::files`) or `file add` (`State::copies`) lists it. `Context::load_state` copies `State::copies` into `Config::added_files`, and `Config::registration` checks both. A copy mode carries the metadata of its `[[files]]` or `[[template_rules]]` entry (`Mode::metadata`). `file add` discovers with a copy of the configuration in which each argument is registered. So `classify` decides with the same rule whether the file can deploy as a copy. `state import` runs the same check (`files::check_copies`) on each imported copy, and the check of `rustow link <path>` (`app::check_link`) on each imported link.

A skipped entry has a kind (`source::SkipKind`): `Present` for a path in the repository that cannot deploy (also a path that rustow cannot read), `NoSource` for a configuration entry or registration that names no source (the path does not exist, is ignored or reserved, or is a directory; `source::not_a_source`). Only a `Present` entry counts as an existing source, so a recorded destination of a `NoSource` one is ORPHANED, as a link in the same case is. A recorded source below a `Present` path (an unreadable directory) is not ORPHANED: rustow cannot tell if it is there. `status` shows it as SKIPPED with the reason of that path (`inspect::orphans`), and discovery gives no warning for a registration below it.

## Directories

`fsops::Dir::walk` resolves a path one component at a time from an open directory (`openat` with `O_NOFOLLOW`; `O_PATH` on Linux, `O_SEARCH` on macOS). A real directory needs no check. A symlink is followed only when `fsops::check_dir` accepts its owner (mode 0) and the directory that it reaches, and when it is still the same symlink after that. `fsops::parent_state` uses the walk for the planner. It returns the real path and the `DirId` (device and inode) of the deepest existing directory. `Deploy::dir_id`, `Step::Unfold` and `Verdict::Remove` keep it; the plan holds no open directory.

The executor and the sudo helper get the directory with `fsops::open_planned(path, dir_id, me)`: a new walk of the planned path that fails on another `DirId`. Every change then goes through that `Dir`: `mkdir` (`mkdirat`), `write` (a temporary file opened `O_CREAT|O_EXCL|O_NOFOLLOW`, then `fchown`, `fchmod`, `fsync`, `renameat` or a no-replace rename), `symlink`, `set_meta`, `remove` (`unlinkat`) and `move_to` (backups; `restrict` first removes the group and other bits of a file that the user could not see). `me` is the effective user; the helper gets the user's uid from the client, so root applies the user's trust rule. `plan::dir_mode` decides the mode of each directory that rustow creates for a destination (`Deploy::dir_mode`, and an unfold); `Dir::mkdir` sets it with `fchmod` after `mkdirat`, so the umask and an inherited setgid bit do not count. `Dir::private` keeps only the owner bits of a cache, state or backup directory. The cache is written through `fsops::private_tree`. The path-based `fsops::write_atomic` (state, `init`, `enc`, `file add`, `state export`) walks the parent for the current user and calls `Dir::write`.

## Content

The plan keeps the content of a regular file as `plan::Content`. A plain copy (`Content::Source`) holds only the path and the length of the repository file; `Desired::File` holds its hash. Rendered and decrypted content (`Content::Generated`) stays in memory from the plan to the write. While one item is planned, its input (the template text, or the ciphertext) and its output are in memory once each: `plan::plaintext` takes the ciphertext and frees it after the decryption, `plan::want_template` drops the template text after the render, and `template::render_at` renders into a buffer the size of the text (MiniJinja's own buffer reserves twice the text, rounded up to a power of two). The compiled template of MiniJinja grows with the number of expressions: with one expression in each line of 100 bytes, it holds about two more times the text. `Hash::copy` streams bytes through one 64 KiB buffer that is zeroed afterwards, and gives the same hash as `Hash::of`. The planner hashes links and copies with it (`fsops::hash_file`), and `Dir::inspect` hashes destinations with it. `Deploy::open_content` opens the content when the item writes it; a source with another length fails. `Dir::write` streams it into the temporary file and checks the hash before the rename: other content fails with "content changed while rustow was running". The sudo client streams the same reader into the frame, and the helper streams the frame into `Dir::write`. `diff` reads content only when the hashes differ, and at most 1 MiB on each side.

## Temporary files

Every temporary name comes from `fsops::temp_name(dirs)`, and the caller passes it to `Dir::write`, `Dir::symlink` or `Dir::move_to`. `app::lock` takes the lock and starts the journal (`Lock::start_journal`, `pending::Pending`): `<state_dir>/<repo-id>.pending`, one JSON path per line. While it runs, `temp_name` appends the path in each of `dirs` and syncs the journal before it returns the name. The executor gets the name of an item from `execute::temp_for` (the destination directory, and the backup directory for a conflict that goes to a backup, which it makes as the user). For the sudo helper, the client does this before the helper starts and sends the name in `Job::Write`. `Pending::start` first removes the leftovers of an interrupted run through a walked directory, or gives the `sudo rm -- <path>` notice for a file in a directory that the user cannot write; an entry stays until its file is gone. `pending` looks at an entry only through its directory walked with the trust rule (`fsops::Dir`); a blocked walk gives a notice without a command. `sudo::call` marks the temporary names of the jobs that the helper answered or never got (`pending::settled`): the helper renames or removes a job's temporary file before it answers. Dropping the `Lock` drops the journal first (`pending::finish`): it keeps only the entries whose files are still there, and removes an empty journal. Consecutive entries of one directory share one walk to it. `doctor` lists the leftovers with `pending::report`.

## Input and output

- rustow reads `rustow.toml`, `.rustowignore`, the state file and `state import` input with `fsops::read_limited`: a regular file, opened with `O_NONBLOCK`, with a size limit.
- rustow starts `gpg`, the sudo program and the editor by the absolute path that `fsops::find_program` returns. The lookup skips relative `PATH` entries.
- Human output passes each path and message through `report::shown`, which escapes control characters, bidirectional controls and backslashes. `Printer::path` does it for destinations. JSON output does not use it. The notices of `pending` are the exception: they hold a shell command (`pending::shell_word`, printable ASCII) that must not be escaped again, so `pending` escapes their paths itself, and `Printer::shown_notice` and `doctor` print them as they are.

## Cache

Default `<repo>/.rustowcache`. Layout: `<cache>/rendered|decrypted/<repo-id>/<scope-id>/<dest>`. Directories are 0700 and files 0600. Before it writes, rustow creates the cache root, or sets its mode to 0700 without a message. It refuses a cache root that is not a directory (it never follows a symlink there) or that another user owns (`fsops::ensure_private_dir`). Only a link uses the cache: a copy (template+copy, decrypt+copy) writes the rendered or decrypted bytes straight to the destination, and its record has no `cache_path`. One exception: when the recorded link of a source used a cache entry and the source is now a copy, the plan checks that entry once (`plan::deploy_step`), so an edit made through the old link is a conflict and not lost.

## State

`$XDG_STATE_HOME/rustow`, else `~/.local/state/rustow` (or `defaults.state_dir`). The file `<repo-id>.json` (mode 0600, directory 0700) is keyed by absolute destination. It also holds the registrations of this machine: `files` (`file add`) and `links` (`link <path>` in copy mode). It holds content hashes, not plaintext. The hashes are sensitive metadata. The file `<repo-id>.lock` stops concurrent runs (exit 4, "another rustow process"). `state::lock` keeps a lock only when the path still names the locked file (device and inode); else it tries again, at most 3 times. When the lock file is removed or replaced during a run (`Lock::replaced`), `app::save` warns before it saves the state. rustow refuses corrupt, foreign or newer state. Move such a file away to continue. You must own the state directory.

The repository id is a hash of the repository path, and the field `repo` records that path. So a moved repository has no state file. `Context::open` then looks for state files whose `repo` does not exist (`state::moved_roots`) and adds a notice for each. `transfer::adopt` (`state adopt`) loads such a file with the checks of `State::load`, sets `repo`, renames `<cache>/rendered|decrypted/<old-id>` to the new id and rewrites each `cache_path` to match, saves it as this repository's state, and keeps the old file as `backups/<old-id>/state-adopted.json`. It removes the old lock file when no run holds it. The next `apply` sees rustow's own links with the old text and updates them.

The file has a `version` (`state::STATE_VERSION`, now 1). `State::load` reads each version from 1 up to it and runs the steps in `state::MIGRATIONS` that the file needs, in order. A newer version fails with "written by a newer rustow". After a migration, `save` first keeps the old bytes as `backups/<repo-id>/state-v<N>.json`. A new field with a default needs no new version: `files` and `links` came that way. An older rustow ignores such a field and drops it when it saves. Raise `STATE_VERSION` only for a change that needs a step, and add one step that turns the old JSON into the new JSON.

## Tests

Integration tests are in `tests/scenario_*.rs`. They use the `Fixture` in `tests/common/mod.rs`. `tests/examples.rs` checks the example repository.
