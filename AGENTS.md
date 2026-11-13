# AGENTS.md

Notes for people and tools that change rustow.

## Commands

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo build --release
```

CI (`.github/workflows/ci.yml`) runs these on Linux and macOS. It also runs `cargo audit`.

Format Markdown with `dprint fmt`. The configuration is in `dprint.json`. CI does not check it.

To release, set `version` in `Cargo.toml`, commit, then push a tag `v<version>`. `.github/workflows/release.yml` runs CI, stops if the tag and the version differ, builds the binaries and publishes a GitHub release.

## Layout

- `src/`: the code. See `docs/architecture.md` for the modules.
- `tests/scenario_*.rs`: integration tests. They run the built binary.
- `tests/common/mod.rs`: the `Fixture`. It makes a temporary HOME with the repository at `~/dotfiles`, a temporary root target and a temporary `XDG_STATE_HOME`. `EDITOR` is `false`.
- `tests/examples.rs`: checks `examples/dotfiles`.
- `docs/`: user documentation. The code and the tests are the source of truth. Describe only implemented behavior.

## Rules

- Use `--root-target` in tests. Never write to the real `/`.
- Tests never run the real `sudo`. The `Fixture` sets `RUSTOW_SUDO=false`. A test that needs the sudo helper uses a fake sudo (see `tests/scenario_s_sudo.rs`).
- GPG tests use disposable `GNUPGHOME` directories and stop their agents. When `CI` is set, a missing `gpg` fails the tests. Locally, they print SKIP.
- The tests expect umask 022. Under umask 002, the files they make are group-writable, and the trust rule accepts that only in a user-private group.
- Some tests read the primary group of the user. They branch on whether it is a user-private group (the user's name, no other members): one branch expects a trusted run, the other a conflict with `chmod g-w`.
- Do not log secrets or file content.
- Keep one planner and one executor for all commands.
- Do not claim secure erasure, transactional rollback or full Go-template compatibility.
- Write documentation in simple, short sentences with active voice.

## Invariants

- Each temporary file that rustow writes next to its final path (atomic writes, `enc`) starts with `fsops::TEMP_PREFIX` (`.rustow-tmp-`). `Ignore::is_reserved` reserves each path component with this prefix, at any depth. No negation or explicit path can deploy it. Use the constant for each new temporary file.
- Each temporary name comes from `fsops::temp_name(dirs)`. While a command holds the lock, it journals the path in each of `dirs` to `<state_dir>/<repo-id>.pending` and syncs it before the file exists (`pending`). Pass every directory where the name may be created (`execute::temp_for`). The sudo helper never makes a name: the client journals it and sends it in `Job::Write`. The next locked command removes the leftovers that the user can remove; for the others it prints `sudo rm -- <path>`. Never run sudo only to clean up. Exceptions: `init` and `state export` hold no lock; `enc`'s set-aside plaintext is not journaled, because it can be the only copy.
- There is one trust rule: `fsops::check_dir`. It checks each path on a link target chain (`check_chain`, `trust_cache`), on the repository chain of each system source (`plan::untrusted_source`), and the owner (mode 0) and the target of each symlink that `fsops::Dir::walk` meets (destination ancestors, target roots). A group may write only when it is the owner's user-private group. Do not add a second rule.
- Every change to a destination goes through an `fsops::Dir` from the walk. The plan keeps paths and `DirId`s, never open directories. The executor and the sudo helper get the directory with `fsops::open_planned` and change names in it (`mkdir`, `write`, `symlink`, `set_meta`, `remove`, `move_to`). Never change a destination through a path string after the plan. The helper walks with the user's uid, not root's.
- `plan::dir_mode` decides the mode of each directory that rustow creates for a destination; `Dir::mkdir` sets it with `fchmod`, so the umask and a setgid parent do not count. Cache, state and backup directories go through `Dir::private`, which keeps only the owner bits. The user makes each backup directory (`execute::temp_in`) before the change; the sudo helper never makes one.
- rustow reads its own input files (configuration, ignore rules, state, `state import`) only with `fsops::read_limited`: a regular file with a size limit.
- rustow starts an external program (`gpg`, the sudo program, the editor) only by the absolute path from `fsops::find_program`.
- Human output passes each name, path and message through `report::shown`. JSON output does not.
- Unfold a folded directory only when the same run deploys every source below it (`plan::hold_partial_unfolds`). Fold a new directory only when the run selects every source below it (`fold::fold_candidates`).
- `Scope::Root` never yields a link mode; the mapping lives in `source.rs` (`scoped_mode`, called by `classify`). Do not add scope checks for modes to the planner.
- Only system files use sudo. The rule lives in `plan::needs_sudo` (deploy) and `plan::removal_needs_sudo` (removal). The helper (`sudo::serve`) runs only `execute::write_dest` and `execute::remove_dest`: it plans nothing and reads no repository, key, cache or state. rustow calls it once per run, after the other items. Do not add a second way to get root. A destination that the user cannot read is `Observed::Unreadable` (metadata only), never an error: the plan compares it with the state, and only `execute::recheck` reads it, as root in the helper, against `Deploy::expect` or the `expect` of a removal.
- The helper protocol is lock-step frames (`sudo::call`, `sudo::serve_on`): per job, one JSON `Header` line (uid, content length, job) and exactly that many raw content bytes; then one `Done` line back before the next frame. The helper reads stdin without a buffer of its own, drains each frame, and stops after a result it cannot write. Content never goes into the JSON. Each result counts on its own.
- A plan item keeps a repository file by reference (`plan::Content::Source`: path, length; the hash is in `Desired::File`). Content streams through `Hash::copy`, and `Dir::write` checks the hash before the rename. Only rendered and decrypted content (`Content::Generated`) stays in memory.
- A problem with one source skips that source with a warning: discovery puts it in `Discovery::skipped`, and the rest deploys with the same exit code. Only global configuration problems exit 2. A path argument that names a skipped file exits 2, as for an ignored file. Do not return `Err` from discovery for one source.
- Configuration errors never print values. `config::toml_error` gives the file, line, column and key path, never the source line. Never show a `[vars]` value in a message. Template errors name the template and the line.
