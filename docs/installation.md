# Installation

Pick one way:

| You have                                         | Use                                                           |
| ------------------------------------------------ | ------------------------------------------------------------- |
| macOS (Apple silicon) or Linux with Homebrew     | [Homebrew](#homebrew): `brew install al-bashkir/tools/rustow` |
| Linux x86_64 or arm64, or macOS on Apple silicon | [A release archive](#download-a-release)                      |
| Any other system, or macOS on Intel              | [Build from source](#build-and-install)                       |

Then [check the installation](#check-the-installation).

## Requirements

- Linux or macOS.
- Rust 1.99 or newer, to build from source.
- GnuPG 2.2 or newer, only if you use `.gpg` sources. The tests use GnuPG 2.4.9. On macOS, run `brew install gnupg`.

age needs no extra program. rustow includes it.

## Homebrew

The [al-bashkir/tools](https://github.com/al-bashkir/homebrew-tools) tap has a formula for macOS on Apple silicon and for Linux on x86_64 and arm64:

```sh
brew install al-bashkir/tools/rustow
```

Or tap first, then install:

```sh
brew tap al-bashkir/tools
brew install rustow
```

The formula installs the release binary and the bash, zsh and fish completions. On Linux, it is the same musl binary as in the release archive. See the musl note in [Download a release](#download-a-release). There is no formula for macOS on Intel. Build from source there.

To update, run `brew upgrade rustow`.

## Download a release

Each [GitHub release](https://github.com/al-bashkir/rustow/releases) has archives for these targets:

- `x86_64-unknown-linux-musl` (static)
- `aarch64-unknown-linux-musl` (static)
- `aarch64-apple-darwin`

Each archive holds the `rustow` binary, `README.md`, `LICENSE` and a `completions/` directory with `rustow.bash`, `_rustow` (zsh) and `rustow.fish`. See [shell completions](#shell-completions). `SHA256SUMS` holds the checksums of the archives. Check the archive, then put the binary on your `PATH`:

```sh
sha256sum --check --ignore-missing SHA256SUMS
tar xzf rustow-v0.1.0-x86_64-unknown-linux-musl.tar.gz
install rustow-v0.1.0-x86_64-unknown-linux-musl/rustow ~/.local/bin/
```

On macOS, use `shasum -a 256 --check --ignore-missing SHA256SUMS`.

The Linux binaries use musl. musl does not load NSS modules, so it can fail to find users and groups from LDAP or SSSD. If an owner in your configuration comes from there, build from source.

## Build and install

```sh
git clone https://github.com/al-bashkir/rustow.git
cd rustow
cargo install --path .
```

To build without installing:

```sh
cargo build --release
```

The binary is `target/release/rustow`.

## Check the installation

```sh
rustow --version
rustow doctor
```

`doctor` reports on the repository, the configuration, the cache and state directories, `gpg` and age identity files. It exits with code 1 when a check fails.

## Shell completions

```sh
rustow completions bash > ~/.local/share/bash-completion/completions/rustow
rustow completions zsh > ~/.zfunc/_rustow
rustow completions fish > ~/.config/fish/completions/rustow.fish
```

The supported shells are `bash`, `zsh`, `fish`, `elvish` and `powershell`.

The script calls `rustow` on each TAB, so `rustow` must be on your `PATH`. Write the script again after an upgrade. The Homebrew formula does this for you.

Commands and paths complete first. Options complete only after you type `-`, and each command offers only its own options and the global options that it takes. A path argument completes repository paths, one directory at a time. Dotfiles show with their leading `.`, so type `.` to see them. Ignored and reserved paths do not show. `enc` offers plaintext files, and `enc edit` offers `.age` and `.gpg` files. Completion finds the repository as a command does, with `$RUSTOW_REPO` or from the current directory. It does not see a `--repo` option on the command line.
