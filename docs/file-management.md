# File management (copies)

By default rustow links files. Use `[[files]]` to deploy a file as a copy instead.

```sh
rustow file add .config/example/settings.json
rustow file list [--json]
rustow file apply
rustow file diff
rustow file remove .config/example/settings.json
```

## add

`file add` registers a plaintext or an encrypted non-template source in `.rustow.toml`. It never deploys. Run `rustow file apply` or `rustow apply` afterwards.

- It is idempotent. A repeat prints "already registered".
- It keeps comments in `.rustow.toml`.
- It exits with code 2 for a template, a directory, an ignored path, a reserved path or an unknown path.
- `file add x.age` makes a `decrypt+copy` source with mode 0600.

## remove

`file remove` removes the registration. The deployed copy stays. If the path is not registered, rustow prints a note and exits with 0.

## Copy rules

- A new copy keeps the source file mode.
- An update keeps the mode of the destination.
- A copy from an encrypted source gets 0600.
- rustow writes a temporary file in the same directory (mode 0600), sets owner and mode, calls `fsync`, then renames it. It never follows a symlink at the destination.
- If you change a copy by hand, rustow reports MODIFIED and a conflict. It does not overwrite your change unless you use `--backup` or `--force`.
- To replace a regular file with a symlink (copy to link), you must use `--backup` or `--force`. To replace rustow's own symlink with a file is automatic.

## Selecting by mode

| Command | Deploys |
| --- | --- |
| `rustow link` | Link only |
| `rustow file apply` | copy and decrypt+copy |
| `rustow template apply` | All templates |
| `rustow apply` | All |

A path argument that matches only sources of other modes is a usage error (exit 2).
