# Quick start

This page takes you from nothing to a working dotfiles repository. It needs about ten minutes.

You need rustow on your `PATH`. See [installation](installation.md). To learn the terms first, read [concepts](concepts.md).

## 1. Create the repository

```sh
mkdir ~/dotfiles && cd ~/dotfiles
git init
rustow init
```

`rustow init` writes a small `rustow.toml`. It also adds the private cache to `.gitignore` and `.rustowignore`. It deploys nothing.

You can put the repository anywhere. rustow finds it from the current directory, from `--repo DIR` or from `$RUSTOW_REPO`.

## 2. Add a file

Copy a dotfile into the repository. Use the same path that it has under your home directory.

```sh
cp ~/.bashrc .bashrc
mkdir -p .config/git && cp ~/.config/git/ignore .config/git/ignore
```

## 3. Look before you deploy

```sh
rustow apply --dry-run
```

rustow lists each action and changes nothing. Your real `~/.bashrc` exists and is not a rustow link, so rustow marks it as a **conflict**.

## 4. Deploy

```sh
rustow apply --backup
```

`--backup` moves your old `~/.bashrc` to a backup directory first. Then rustow makes `~/.bashrc` a symlink to the file in the repository. Without `--backup`, rustow leaves the conflicting file, deploys the other items and exits with code 3.

Check the result:

```sh
rustow status
```

`status` lists each item that needs attention, then a summary. It exits with 0 when all is clean and with 1 when something needs attention. Add `-v` to list the clean items too.

## 5. Save to Git

```sh
git add -A && git commit -m "first dotfiles"
```

rustow never runs Git. You commit and push yourself.

## 6. Use the repository on another machine

```sh
git clone <your-url> ~/dotfiles
cd ~/dotfiles
rustow apply --dry-run
rustow apply
```

## Add more than links

### A template

Use a template when a file differs between machines. Copy the file with the suffix `.tmpl`, then add placeholders:

```sh
cp ~/.gitconfig .gitconfig.tmpl
```

```jinja
[user]
    name = {{ vars.git.user_name }}
{%- if os == "macos" %}
    fsmonitor = true
{%- endif %}
```

Set the values in `rustow.toml`:

```toml
[vars.git]
user_name = "Ada Lovelace"
```

`rustow apply` now renders `.gitconfig.tmpl` to `~/.gitconfig`. See [templates](templates.md).

### A secret

Make an age key. It prints the public key (`age1...`). Put that key in `rustow.toml`. Then encrypt the file:

```sh
age-keygen -o ~/.config/rustow/age/identity.txt
chmod 600 ~/.config/rustow/age/identity.txt
```

```toml
[encryption]
recipients = ["age1..."]
identity_files = ["~/.config/rustow/age/identity.txt"]
```

```sh
rustow enc .ssh/secret.conf
```

rustow writes `.ssh/secret.conf.age` and removes the plaintext. Git history keeps any older plaintext. See [encryption](encryption.md) for GnuPG and for editing the file later.

### A file in `/etc`

Put the file under `rustowroot/`, for example `rustowroot/etc/example/example.conf.tmpl`. rustow runs as you and calls `sudo` only for the files that you cannot write. See [system files](system-files.md).

## Try the example repository

The directory [`examples/dotfiles`](../examples/dotfiles) has Bash, Zsh, Git, Neovim, SSH and system files. Deploy it into a temporary directory. This is safe, because nothing touches your real home directory.

```sh
mkdir -p /tmp/home /tmp/root
rustow --repo examples/dotfiles check
rustow --repo examples/dotfiles --target /tmp/home --root-target /tmp/root apply --dry-run
rustow --repo examples/dotfiles --target /tmp/home --root-target /tmp/root apply
rustow --repo examples/dotfiles --target /tmp/home --root-target /tmp/root status -v
```

| Repository path                            | Destination                 | Mode                            |
| ------------------------------------------ | --------------------------- | ------------------------------- |
| `.bashrc`, `.zshrc`                        | `~/.bashrc`, `~/.zshrc`     | link                            |
| `.config/git/ignore`                       | `~/.config/git/ignore`      | link                            |
| `.config/nvim/init.lua`                    | `~/.config/nvim/init.lua`   | link                            |
| `.gitconfig.tmpl`                          | `~/.gitconfig`              | template+link                   |
| `.ssh/config.tmpl`                         | `~/.ssh/config`             | template+copy (rule: mode 0600) |
| `rustowroot/etc/example/example.conf.tmpl` | `/etc/example/example.conf` | template+copy (rule: mode 0644) |

`check` works offline. It reads the configuration and the ignore rules, finds collisions and renders plaintext templates. It never decrypts.

## Next steps

- If something fails, see [troubleshooting](troubleshooting.md).
- Deploy a file as a copy: [file management](file-management.md).
- Leave files out: [ignore rules](ignore.md).
- All settings: [configuration](configuration.md).
- All commands: [command reference](cli.md).
