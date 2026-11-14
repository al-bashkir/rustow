# File management (copies)

By default rustow links home files. Some files must be real files, not links. Examples are files that a program rewrites or that need a fixed mode. Register such a file as a copy in one of these ways:

- Run `rustow file add <path>`. This registers the file on this machine only.
- List the file in `[[files]]` in `rustow.toml`. This applies on every machine.
- Set `[defaults] mode = "copy"`. Then `apply` links no file by itself. You add each file with `file add` or `rustow link <path>`. See [copy mode](configuration.md#copy-mode).

A system file under `rustowroot/` that is not a template must be registered. Until then, rustow skips it with a warning. See [system files](system-files.md).

```sh
rustow file add .config/example/settings.json
rustow file list [--json]
rustow file apply
rustow file diff
rustow file remove .config/example/settings.json
```

## add

`file add` registers a plaintext or an encrypted non-template source in the state file of this machine (`<state_dir>/<repo-id>.json`). It does not change `rustow.toml`. It never deploys. Run `rustow file apply` or `rustow apply` afterwards.

- It is idempotent. A repeat prints "already registered". A path that `[[files]]` lists is also "already registered".
- It exits with code 2 for a template, a directory, an ignored path, a reserved path or an unknown path. It also exits with code 2 for a file that cannot deploy as a copy, for example a symlink or a name with nested suffixes such as `x.age.gpg`. The message gives the reason. It registers nothing then.
- `file add x.age` makes a `decrypt+copy` source with mode 0600.
- It registers a system file also while other system files are not registered yet.

## State or `rustow.toml`

There are two lists of copies. rustow deploys a source as a copy when either list has it.

| List                       | Written by        | Scope                                  |
| -------------------------- | ----------------- | -------------------------------------- |
| state file                 | `rustow file add` | this machine and this user             |
| `[[files]]` in rustow.toml | you, by hand      | every machine that uses the repository |

Use `[[files]]` for a file that every machine must copy. The state belongs to the user who runs rustow. To move the state registrations to another machine, run `rustow state export FILE` there and `rustow state import FILE` here. See [cli](cli.md#state-export-and-import).

A registration whose file no longer exists gives a warning, for example "warning: gone.json: `rustow file add` registration does not exist (remove the entry, e.g. with `rustow file remove`)".

## remove

`file remove` removes the registration from the state and from `[[files]]` in `rustow.toml`. It keeps comments and CRLF line endings in `rustow.toml`. The deployed copy stays. If the path is not registered, rustow prints a note and exits with 0.

## list

`file list` shows the sources of both lists, each with its destination and mode. The text output shortens the home directory to `~`. `file list --json` gives `{"files": [{"source", "dest", "mode"}]}` with the absolute destination, as all JSON output does. A registration cannot deploy when its source is missing, is a directory, is ignored, or is a [skipped source](cli.md#skipped-sources). It then has no destination: `dest` is null, and `mode` is "invalid: " and the reason. The text line shows the source and the reason only, for example `gone (invalid: rustow.toml [[files]] entry does not exist …)`.

## Copy rules

- A new copy keeps the source file mode.
- An update keeps the mode, owner and group of the destination (template metadata can set them explicitly).
- A copy from an encrypted source gets 0600.
- rustow writes a temporary file in the same directory (mode 0600), sets owner and mode, calls `fsync`, then renames it. It never follows a symlink at the destination.
- The plan keeps only the hash and the length of the source. rustow reads the source again when it writes the copy, a little at a time, so a large file needs little memory. When the source changed after the plan, the item fails with "content changed while rustow was running; left unchanged", and the destination stays as it was. Run the command again.
- If you change a copy by hand, rustow reports MODIFIED and a conflict. It does not overwrite your change unless you use `--backup` or `--force`.
- To replace a regular file with a symlink (copy to link), you must use `--backup` or `--force`. To replace rustow's own symlink with a file is automatic.
- An exception: you edited a cache entry through rustow's link, then you switch the source to a copy (`file add x.age`, or a `[[template_rules]]` entry). Then `apply` reports a conflict: "cache entry: destination was modified since rustow last applied it". `--backup` saves your edit under `<state_dir>/backups/<repo-id>/<time>/cache/<dest>`. `--force` discards it.

## Selecting by mode

| Command                 | Deploys                         |
| ----------------------- | ------------------------------- |
| `rustow link`           | Link only (never a system file) |
| `rustow file apply`     | copy and decrypt+copy           |
| `rustow template apply` | All templates                   |
| `rustow apply`          | All                             |

A path argument that matches only sources of other modes is a usage error (exit 2).
