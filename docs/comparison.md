# Comparison with other tools

This page compares rustow with five dotfiles managers: [GNU Stow](https://www.gnu.org/software/stow/), [chezmoi](https://www.chezmoi.io/), [dotter](https://github.com/SuperCuber/dotter), [yadm](https://yadm.io/) and [Tuckr](https://github.com/RaphGL/Tuckr).

Each tool solves the same problem in a different way. None is best for everyone. The tables show what each tool does by design, so that you can choose.

The other tools change over time. This page was written in November 2026 from their public documentation. Check their pages for the latest facts, and [open an issue](https://github.com/al-bashkir/rustow/issues) if something here is wrong.

## In short

| Tool    | The idea                                                                 | Choose it when                                                           |
| ------- | ------------------------------------------------------------------------ | ------------------------------------------------------------------------ |
| rustow  | A flat repository that mirrors `~`. Links by default, more on request.   | You like Stow's links but you also need templates, secrets and `/etc`.   |
| Stow    | One directory per package. A farm of symlinks.                           | You only need symlinks and want the smallest, oldest, most common tool.  |
| chezmoi | A source directory with special names. It copies files and renders them. | You want the most features, Windows support or password manager support. |
| dotter  | A TOML file maps repository files to destinations.                       | You want explicit control of each file, with templates, in a small tool. |
| yadm    | Git itself, with your home directory as the work tree.                   | You know Git well and want to keep files in place, with few extras.      |
| Tuckr   | Stow-like groups, with hooks and an experimental secrets feature.        | You want a Rust Stow with groups per program and per OS.                 |

## Feature table

"Yes" means built in. "No" means not built in. "Partly" has a note below the table.

| Feature                   | rustow                        | Stow              | chezmoi            | dotter           | yadm                       | Tuckr             |
| ------------------------- | ----------------------------- | ----------------- | ------------------ | ---------------- | -------------------------- | ----------------- |
| Language                  | Rust                          | Perl              | Go                 | Rust             | Bash                       | Rust              |
| Linux and macOS           | Yes                           | Yes               | Yes                | Yes              | Yes                        | Yes               |
| Windows                   | No                            | No                | Yes                | Yes              | Yes                        | Yes               |
| Deploys files as          | Links (default) or copies     | Links             | Copies (or links)  | Links or copies  | Files in place             | Links             |
| Config file needed        | No (optional)                 | No                | No (optional)      | Yes              | No (optional)              | No                |
| Templates                 | Yes (MiniJinja)               | No                | Yes (Go templates) | Yes (Handlebars) | Yes (built-in, ESH, Jinja) | No                |
| Different files per host  | Templates, `[[global_rules]]` | Separate packages | Templates          | Variables        | Alternate files `##`       | Groups per OS     |
| Encryption of files       | age, GnuPG                    | No                | age, GnuPG, others | No               | GnuPG, OpenSSL, others     | Experimental      |
| Password manager support  | No                            | No                | Yes                | No               | No                         | No                |
| Files outside `$HOME`     | Yes, `rustowroot/` with sudo  | Yes (manual)      | Not the main use   | Any target path  | Not the main use           | Yes (`^` prefix)  |
| Show changes before apply | Yes, `--dry-run` and `diff`   | Yes (`-n`)        | Yes, `diff`        | Yes, `-d`        | Yes, via Git               | Yes, `status`     |
| Records what it deployed  | Yes, state file               | No                | Yes                | Yes, cache       | Git                        | Yes, checks links |
| Conflict handling         | Skip, abort, backup, force    | Abort             | Prompt             | Skip or force    | Git rules                  | Not stated        |
| Run scripts and hooks     | No                            | No                | Yes                | Yes              | Yes (bootstrap, hooks)     | Yes (hooks)       |

Notes:

- **Stow** has no state. The links are the state. It stops before it changes anything when it finds a conflict.
- **chezmoi** can also make symlinks. Copies are the default.
- **yadm** keeps your files where they are. It has no deploy step. The Git work tree is your home directory.
- **dotter** decides per file: a file with `{{` in it is a template and gets copied. Other files become links.
- **Tuckr** says that its secrets feature is work in progress and not for production use.
- **rustow** has no Windows support and no hooks or run scripts. Neither is implemented.

## How rustow differs

### GNU Stow

rustow takes the symlink model from Stow, including directory folding. The differences:

- **Layout.** Stow wants a directory for each package: `stow/vim/.vimrc`. rustow has one flat tree that mirrors `~`. You use `.rustowignore` and `[[global_rules]]` to leave files out on some hosts.
- **Beyond links.** Stow has no templates, no encryption and no copies. rustow has all three.
- **State.** Stow keeps no record. rustow keeps a state file and also checks the file system before every change. So it knows which links are its own and can remove them.
- **Conflicts.** Stow stops. rustow deploys everything that is safe and lists the rest. `--on-conflict abort` gives you the Stow behavior.
- **System files.** With Stow you run `sudo stow -t /` yourself. rustow runs as you and asks `sudo` only for the files that need it.

### chezmoi

rustow takes templates, encryption and state from chezmoi. The differences:

- **Names.** chezmoi encodes attributes in file names: `dot_`, `private_`, `executable_`. rustow keeps the real names. It reads only the suffixes `.tmpl`, `.age` and `.gpg`, and the metadata in `rustow.toml` or in a template header.
- **Links.** chezmoi copies by default. rustow links by default, so an edit in `~/.bashrc` is an edit in the repository at once. Set `mode = "copy"` for the chezmoi way.
- **Templates.** chezmoi uses Go templates with many functions. rustow uses MiniJinja with variables from `rustow.toml` and facts about the machine. rustow cannot read a password manager.
- **Scope.** chezmoi does more: Windows, password managers, external archives, run-once scripts, and a larger community. rustow does less, and tries to do it with fewer surprises.

### dotter

dotter and rustow are both small Rust tools with templates and links. The differences:

- **Mapping.** dotter lists each file in `global.toml`. rustow finds files by their place in the repository. You do not list them.
- **Encryption.** dotter has none built in. rustow has age and GnuPG.
- **System files.** rustow has a clear model for `/etc` with `sudo`. See [system files](system-files.md).
- **Hooks.** dotter runs pre and post deploy scripts. rustow does not.
- **Windows.** dotter runs on Windows. rustow does not.

### yadm

yadm is a thin layer on Git. rustow is a deploy tool.

- **Where files live.** yadm keeps the real files in `$HOME`. Its bare Git directory is in `~/.local/share/yadm`. rustow keeps a normal repository and deploys from it. You can check the repository out anywhere and rustow can undo its work.
- **Per-host files.** yadm uses alternate files with a `##` suffix. rustow uses templates and `[[global_rules]]`.
- **Bootstrap.** yadm can run a bootstrap script. rustow has no scripts.
- **Safety.** yadm gives you Git, with all of its power and all of its sharp edges. rustow plans, checks and reports before it changes a file.

### Tuckr

Tuckr is the closest to rustow in spirit: a Rust tool with the Stow model.

- **Layout.** Tuckr uses `Configs/<group>/…`. rustow uses one tree. Tuckr groups can be tied to an OS (`_linux`, `_macos`). rustow uses `[[global_rules]]`.
- **Templates.** Tuckr documents none. rustow has MiniJinja templates.
- **Secrets.** Tuckr's secrets are experimental. rustow supports age and GnuPG sources, and tests them.
- **Hooks.** Tuckr runs hooks with `set` and `unset`. rustow has no hooks.
- **Licence.** Tuckr is GPL-3.0. rustow is MIT.

## When not to use rustow

- You use Windows. Use chezmoi, dotter or yadm.
- You need to run scripts after a deploy, for example to install packages. Use chezmoi or yadm, or run your script after `rustow apply`.
- You need to read secrets from a password manager. Use chezmoi.
- You want the smallest tool for plain links and nothing else. Use Stow.

## Sources

- [chezmoi comparison table](https://www.chezmoi.io/comparison-table/)
- [Tuckr documentation](https://raphgl.github.io/Tuckr/)
- [dotter README](https://github.com/SuperCuber/dotter)
- [yadm README](https://github.com/yadm-dev/yadm)
- [GNU Stow manual](https://www.gnu.org/software/stow/manual/stow.html)
