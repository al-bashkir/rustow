<h1 align="center"><img src="assets/logo.svg" alt="rustow" width="360"></h1>

<p align="center">A dotfiles manager for Linux and macOS, written in Rust.</p>

> [!NOTE]
> AI disclosure: the code, the tests and the documentation of rustow were written with Claude Opus, an AI model from Anthropic. Run `rustow apply --dry-run` first, and read the [Limits](#limits), before you trust it with files that matter.

rustow keeps your dotfiles in one Git repository and deploys them to your machine. The repository looks like your home directory. rustow links, copies or renders each file to the right place, and it never overwrites your files without a check.

```text
~/dotfiles/                         deploys to
├── .bashrc                    →    ~/.bashrc                (symlink)
├── .gitconfig.tmpl            →    ~/.gitconfig             (template)
├── .config/nvim/init.lua      →    ~/.config/nvim/init.lua  (symlink)
├── .ssh/id_ed25519.age        →    ~/.ssh/id_ed25519        (decrypted)
└── rustowroot/etc/app.conf.tmpl →  /etc/app.conf            (template, via sudo)
```

## Why rustow

- **Simple by default.** One repository, no naming scheme. A file deploys as a symlink, like GNU Stow.
- **More when you need it.** Add a `.tmpl` suffix for a template. Add `.age` or `.gpg` for an encrypted file. Register a file as a copy with `rustow file add`.
- **Safe.** rustow checks the real file system before each change. It leaves a conflicting file alone, deploys the rest and tells you what it skipped. `--dry-run` shows every action first.
- **System files too.** Files under `rustowroot/` deploy to `/`. You run rustow as yourself. It asks `sudo` only for the files that you cannot write.
- **Scriptable.** Each command has an exit code and a `--json` output.

## Why this project exists

rustow exists because the other tools did not offer the features that its author needed. It was written mainly for personal use. The code is public in case it is useful to someone else too.

Not sure if rustow fits you? See the [comparison with Stow, chezmoi, dotter, yadm and Tuckr](docs/comparison.md).

## Install

```sh
brew install al-bashkir/tools/rustow
```

This works on macOS (Apple silicon) and on Linux. For other ways, such as release archives or `cargo install`, see [Installation](docs/installation.md).

## Try it in two minutes

```sh
mkdir ~/dotfiles && cd ~/dotfiles
git init
rustow init                 # create rustow.toml and the ignore files
cp ~/.bashrc .bashrc        # put a file in the repository
rustow apply --dry-run      # show what would happen
rustow apply --backup       # deploy; save the old ~/.bashrc as a backup
rustow status               # check that all is in place
```

The [Quick start](docs/quickstart.md) explains each step. A complete example repository is in [examples/dotfiles](examples/dotfiles).

## Documentation

- [Quick start](docs/quickstart.md): your first repository, step by step.
- [Concepts](docs/concepts.md): how files map to destinations, and how they deploy.
- [Comparison with other tools](docs/comparison.md): Stow, chezmoi, dotter, yadm and Tuckr.
- [Full documentation index](docs/README.md): every page, listed by goal.

## Limits

- rustow has no global rollback. It saves the state file once, at the end of the run. After SIGKILL, the next `apply` adopts the destinations that are correct.
- rustow does not erase data securely. Removed plaintext can stay in Git history, backups and deployed files.
- The template language is MiniJinja. It is not fully compatible with Go templates.

## License

MIT. See [LICENSE](LICENSE).
