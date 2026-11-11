# Installation

## Requirements

- Linux or macOS.
- Rust 1.89 or newer, to build from source.
- GnuPG 2.2 or newer, only if you use `.gpg` sources. The tests use GnuPG 2.4.9. On macOS, run `brew install gnupg`.

age needs no extra program. rustow includes it.

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
