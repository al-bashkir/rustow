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
- GPG tests use disposable `GNUPGHOME` directories and stop their agents. When `CI` is set, a missing `gpg` fails the tests. Locally, they print SKIP.
- The tests expect umask 022. Under umask 002, the files they make are group-writable, and the trust rule accepts that only in a user-private group.
- Some tests read the primary group of the user. They branch on whether it is a user-private group (the user's name, no other members): one branch expects a trusted run, the other a conflict with `chmod g-w`.
- Do not log secrets or file content.
- Keep one planner and one executor for all commands.
- Do not claim secure erasure, transactional rollback or full Go-template compatibility.
- Write documentation in simple, short sentences with active voice.

## Invariants

- Each temporary file that rustow writes next to its final path (atomic writes, `enc`) starts with `fsops::TEMP_PREFIX` (`.rustow-tmp-`). `Ignore::is_reserved` reserves each path component with this prefix, at any depth. No negation or explicit path can deploy it. Use the constant for each new temporary file.
- There is one trust rule: `fsops::check_dir`. It checks each path on a link target chain (`check_chain`, `trust_cache`) and the target of a symlinked destination ancestor (`parent_state`). A group may write only when it is the owner's user-private group. Do not add a second rule.
- Unfold a folded directory only when the same run deploys every source below it (`plan::hold_partial_unfolds`). Fold a new directory only when the run selects every source below it (`fold::fold_candidates`).
- `Scope::Root` never yields a link mode; the mapping lives in `source.rs` (`scoped_mode`, called by `classify`). Do not add scope checks to the planner.
- A problem with one source skips that source with a warning: discovery puts it in `Discovery::skipped`, and the rest deploys with the same exit code. Only global configuration problems exit 2. A path argument that names a skipped file exits 2, as for an ignored file. Do not return `Err` from discovery for one source.
- Configuration errors never print values. `config::toml_error` gives the file, line, column and key path, never the source line. Never show a `[vars]` value in a message. Template errors name the template and the line.
