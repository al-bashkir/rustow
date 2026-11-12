# Quick start

This page uses the example repository in `examples/dotfiles`. It has Bash, Zsh, Git, Neovim, SSH and system files.

## 1. Look at the layout

The repository root mirrors your home directory.

| Repository path                            | Destination                 | Mode                            |
| ------------------------------------------ | --------------------------- | ------------------------------- |
| `.bashrc`, `.zshrc`                        | `~/.bashrc`, `~/.zshrc`     | link                            |
| `.config/git/ignore`                       | `~/.config/git/ignore`      | link                            |
| `.config/nvim/init.lua`                    | `~/.config/nvim/init.lua`   | link                            |
| `.gitconfig.tmpl`                          | `~/.gitconfig`              | template+link                   |
| `.ssh/config.tmpl`                         | `~/.ssh/config`             | template+copy (rule: mode 0600) |
| `rustowroot/etc/example/example.conf.tmpl` | `/etc/example/example.conf` | template+copy (rule: mode 0644) |

`rustow.toml` holds the variables and the template rules. `.rustowignore` and `.gitignore` exclude the private cache.

## 2. Check the repository

```sh
rustow --repo examples/dotfiles check
```

`check` works offline. It reads the configuration and the ignore rules, finds collisions and renders plaintext templates. It never decrypts.

## 3. Try it in a safe place

Use a temporary target and a temporary root target:

```sh
mkdir -p /tmp/home /tmp/root
rustow --repo examples/dotfiles --target /tmp/home --root-target /tmp/root apply --dry-run
rustow --repo examples/dotfiles --target /tmp/home --root-target /tmp/root apply
rustow --repo examples/dotfiles --target /tmp/home --root-target /tmp/root status -v
```

`apply --dry-run` lists each action and changes nothing. `status -v` also lists clean items.

## 4. Use your own repository

```sh
mkdir ~/dotfiles && cd ~/dotfiles
git init
rustow init
cp ~/.bashrc .bashrc
rustow apply --dry-run
rustow apply --backup
```

`rustow apply` finds that `~/.bashrc` exists and is not a rustow symlink. This is a conflict. By default, rustow leaves the file, deploys the other items and lists the conflict at the end. Use `--backup` to move the old file into the backup directory. Use `--force` to replace it.

## 5. Next steps

- Add a template: [templates](templates.md).
- Deploy a file as a copy: [file management](file-management.md).
- Keep a secret in Git: [encryption](encryption.md).
- Deploy a file to `/etc`: [system files](system-files.md).
