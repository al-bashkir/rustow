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

## Layout

- `src/`: the code. See `docs/architecture.md` for the modules.
- `tests/scenario_*.rs`: integration tests. They run the built binary.
- `tests/common/mod.rs`: the `Fixture`. It makes a temporary HOME with the repository at `~/dotfiles`, a temporary root target and a temporary `XDG_STATE_HOME`. `EDITOR` is `false`.
- `tests/examples.rs`: checks `examples/dotfiles`.
- `docs/`: user documentation. The code and the tests are the source of truth. Describe only implemented behavior.

## Rules

- Use `--root-target` in tests. Never write to the real `/`.
- GPG tests use disposable `GNUPGHOME` directories and stop their agents. When `CI` is set, a missing `gpg` fails the tests. Locally, they print SKIP.
- Do not log secrets or file content. Error messages name templates and lines, not values.
- Keep one planner and one executor for all commands.
- Do not claim secure erasure, transactional rollback or full Go-template compatibility.
- Write documentation in simple, short sentences with active voice.
