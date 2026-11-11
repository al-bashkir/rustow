# rustow

rustow is a dotfiles manager for Linux and macOS. It is written in Rust.

The repository root mirrors your home directory. The `.rustowroot/` directory mirrors `/`. rustow deploys each file as a symlink, a copy, or a rendered template. It can keep secrets encrypted in the repository with age or GnuPG.

## Features

- Symlinks by default. Copies, templates and encrypted files are opt-in by file name or by configuration.
- Templates use MiniJinja. Undefined variables are errors.
- Encrypted sources: age (built in) and GnuPG (installed `gpg`).
- System files under `.rustowroot/`. rustow never runs `sudo`.
- Safe conflict handling. By default rustow changes nothing when any item conflicts.
- State file per repository. `status`, `diff` and `check` show differences. They exit with code 1 when they find some.
- `.rustowignore` with gitignore syntax.

## Install

```sh
cargo install --path .
```

See [docs/installation.md](docs/installation.md).

## Quick start

```sh
cd ~/dotfiles
rustow init
rustow apply --dry-run
rustow apply
```

An example repository is in [examples/dotfiles](examples/dotfiles). See [docs/quickstart.md](docs/quickstart.md).

## Documentation

- [Installation](docs/installation.md)
- [Quick start](docs/quickstart.md)
- [Initialization](docs/initialization.md)
- [Configuration](docs/configuration.md)
- [Ignore rules](docs/ignore.md)
- [Templates](docs/templates.md)
- [File management (copies)](docs/file-management.md)
- [Encryption](docs/encryption.md)
- [System files](docs/system-files.md)
- [Command reference](docs/cli.md)
- [Architecture](docs/architecture.md)
- [Security](docs/security.md)
- [Troubleshooting](docs/troubleshooting.md)
- [Developer notes (AGENTS.md)](AGENTS.md)

## Limits

- rustow has no global rollback. It records state for each successful item.
- rustow does not erase data securely. Removed plaintext can stay in Git history, backups and deployed files.
- The template language is MiniJinja. It is not fully compatible with Go templates.

## License

MIT.
