# Concepts

This page explains the ideas that you need to use rustow. The other pages go into detail.

## The repository mirrors your home directory

A rustow repository is a normal Git repository. A file in it deploys to the same relative path in your home directory.

| In the repository                      | On your machine           |
| -------------------------------------- | ------------------------- |
| `.bashrc`                              | `~/.bashrc`               |
| `.config/nvim/init.lua`                | `~/.config/nvim/init.lua` |
| `rustowroot/etc/example/app.conf.tmpl` | `/etc/example/app.conf`   |

- There are no "packages" and no special names for directories.
- The directory `rustowroot/` mirrors `/` instead of your home directory. See [system files](system-files.md).
- Path arguments of commands are always relative to the repository, not to your current directory.

## The file name selects the mode

rustow looks at the name of each file and decides how to deploy it.

| File name         | What rustow does                                                           | Mode            |
| ----------------- | -------------------------------------------------------------------------- | --------------- |
| `.bashrc`         | Links the destination to the repository file.                              | `link`          |
| `.gitconfig.tmpl` | Renders the template. The result goes to a private cache. rustow links it. | `template+link` |
| `id_ed25519.age`  | Decrypts the file into a private cache. rustow links it.                   | `decrypt+link`  |
| `.gnupg/x.gpg`    | The same, with GnuPG.                                                      | `decrypt+link`  |

The destination name drops the `.tmpl`, `.age` or `.gpg` suffix.

## Links are the default, copies are opt-in

rustow deploys a **copy** instead of a link in these cases:

- You register the file with `rustow file add <path>`, or list it in `[[files]]` in `rustow.toml`. See [file management](file-management.md).
- A template has a `[[template_rules]]` entry or a meta block. Then it needs a mode, an owner or a group, so it must be a file. See [templates](templates.md).
- The file is under `rustowroot/`. System files are never links.
- You set `mode = "copy"` in `[defaults]`. Then you add each file by hand. See [copy mode](configuration.md#copy-mode).

All six modes are `link`, `copy`, `template+link`, `template+copy`, `decrypt+link` and `decrypt+copy`. `rustow template list` shows how each of your templates deploys.

## A run has two steps

1. **Plan.** rustow reads the repository, the configuration and the real file system. It decides one action for each destination: create, update, remove, adopt, skip or conflict. It changes nothing. `--dry-run` stops here and prints the plan.
2. **Execute.** rustow runs the plan. It checks each destination again just before it changes it. It saves the result in the state file at the end.

One planner and one executor serve all commands.

## Conflicts

A conflict is a destination that rustow did not create and cannot prove to be its own. Examples are an existing `~/.bashrc` or a copy that you edited by hand.

By default rustow leaves each conflict, deploys the rest and exits with code 3. You choose another policy:

| Option                | Effect                                                  |
| --------------------- | ------------------------------------------------------- |
| (default) `skip`      | Leave the conflicts. Deploy the other items.            |
| `--on-conflict abort` | Change nothing if any item conflicts.                   |
| `--backup`            | Move the old file to the backup directory. Then deploy. |
| `--force`             | Replace the old file.                                   |

See [troubleshooting](troubleshooting.md#exit-code-3-conflicts) for how to resolve each kind of conflict.

## State and cache

- The **state file** records what rustow deployed on this machine: destinations and content hashes. It lives in `~/.local/state/rustow` (or `$XDG_STATE_HOME/rustow`). It is not in the repository. rustow never trusts it alone: it checks the real file system before every change.
- The **cache** holds rendered templates and decrypted files that rustow links to. It lives in `<repo>/.rustowcache` and `rustow init` keeps it out of Git. Its directories have mode 0700.

## Exit codes

| Code | Meaning                                        |
| ---- | ---------------------------------------------- |
| 0    | Success, or everything is clean.               |
| 1    | `status`, `diff` or `check` found differences. |
| 2    | Usage or configuration error.                  |
| 3    | Conflicts. The other items were deployed.      |
| 4    | Runtime error or partial failure.              |

A problem with one source is not an error. rustow skips that source with a warning and deploys the rest. See the [command reference](cli.md#exit-codes) for all codes.

## Where to go next

- Do the [quick start](quickstart.md).
- Read [templates](templates.md) or [encryption](encryption.md) when you need them.
- Look up a command in the [command reference](cli.md).
