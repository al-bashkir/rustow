<h1 align="center"><img src="assets/logo.svg" alt="rustow" width="360"></h1>

rustow is a dotfiles manager for Linux and macOS. It is written in Rust.

The repository root mirrors your home directory. The `.rustowroot/` directory mirrors `/`. rustow deploys each file as a symlink, a copy, or a rendered template. It can keep secrets encrypted in the repository with age or GnuPG.

## Features

- Symlinks by default. Copies, templates and encrypted files are opt-in by file name or by configuration.
- Templates use MiniJinja. Undefined variables are errors.
- Encrypted sources: age (built in) and GnuPG (installed `gpg`).
- System files under `.rustowroot/`. They deploy only as copies, never as symlinks. You run rustow as yourself; it asks `sudo` only for the system files that you cannot write.
- Safe conflict handling. By default rustow changes nothing when any item conflicts or fails to plan.
- A problem with one source does not stop the run. rustow skips that source with a warning and deploys the rest.
- State file per repository. `status`, `diff` and `check` show differences. They exit with code 1 when they find some.
- `.rustowignore` with gitignore syntax.

## Install

With [Homebrew](https://brew.sh) on macOS (Apple silicon) or Linux:

```sh
brew install al-bashkir/tools/rustow
```

Or download an archive from [GitHub releases](https://github.com/al-bashkir/rustow/releases). There are static binaries for Linux x86_64 and arm64, and a binary for macOS on Apple silicon.

On other systems, build from source with Rust 1.99 or newer:

```sh
cargo install --locked --git https://github.com/al-bashkir/rustow
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

## Differences from GNU Stow and chezmoi

rustow takes the symlink model of GNU Stow and the templates, copies, encryption and state of chezmoi. It is a separate program, not a clone of either.

| Topic              | GNU Stow                                  | chezmoi                                                      | rustow                                                                                                                                 |
| ------------------ | ----------------------------------------- | ------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------- |
| Repository layout  | One directory per package                 | Source names with attribute prefixes (`dot_`, `private_`, …) | Flat: the repository mirrors `~`; `.rustowroot/` mirrors `/`                                                                           |
| Default deployment | Symlink                                   | Copy                                                         | Symlink                                                                                                                                |
| Copies             | No                                        | Default                                                      | Per file, with `rustow file add`                                                                                                       |
| Templates          | No                                        | Go `text/template`                                           | MiniJinja, rendered into a repository-local cache, then linked; copied when the template has mode, owner or group, or is a system file |
| Encryption         | No                                        | age, GPG and others                                          | age (built in) and GPG (installed `gpg`), decrypted into a private cache                                                               |
| System files       | A separate stow directory with target `/` | Not the main use                                             | `.rustowroot/`, copies only; run as yourself, sudo only for files you cannot write                                                     |
| Conflicts          | Stop                                      | Prompt by default                                            | Stop and change nothing by default; `skip`, `backup` or `overwrite` on request                                                         |
| State              | None; the links are the state             | Persistent state                                             | Persistent state, plus a check of the real filesystem before every change                                                              |
| Directory folding  | On by default                             | No                                                           | On by default, never directly under `~` or `~/.local`; per file or directory with `[[file_rules]]`                                     |

## Limits

- rustow has no global rollback. It records state for each successful item.
- rustow does not erase data securely. Removed plaintext can stay in Git history, backups and deployed files.
- The template language is MiniJinja. It is not fully compatible with Go templates.

## License

MIT. See [LICENSE](LICENSE).
