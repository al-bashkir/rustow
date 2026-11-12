# Installation

## Requirements

- Linux or macOS.
- Rust 1.99 or newer, to build from source.
- GnuPG 2.2 or newer, only if you use `.gpg` sources. The tests use GnuPG 2.4.9. On macOS, run `brew install gnupg`.

age needs no extra program. rustow includes it.

## Download a release

Each [GitHub release](https://github.com/al-bashkir/rustow/releases) has archives for these targets:

- `x86_64-unknown-linux-musl` (static)
- `aarch64-unknown-linux-musl` (static)
- `aarch64-apple-darwin`

Each archive holds the `rustow` binary, `README.md` and `LICENSE`. `SHA256SUMS` holds the checksums of the archives. Check the archive, then put the binary on your `PATH`:

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
