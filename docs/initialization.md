# Initialization

`rustow init` prepares a new or existing repository. It does not deploy anything. It never runs Git.

```sh
rustow init [--dry-run]
```

## Which repository

rustow picks the repository in this order:

1. `--repo DIR`.
2. `$RUSTOW_REPO`.
3. The nearest ancestor of the current directory that has `rustow.toml` or `.rustowignore`. The old names `.rustow.toml` and `.rustowroot` also mark a repository, but every command then stops with exit 2 and asks you to rename them.
4. The current directory, for `init` only. Every other command stops with exit 2 and asks you to run `rustow init`, pass `--repo` or set `RUSTOW_REPO`. This prevents a bare `rustow` from deploying an unrelated directory into your home.

rustow refuses `/` as repository. It refuses a target directory inside the repository.

Path arguments of commands are always relative to the repository. They are never relative to the current directory.

## What init does

- Creates a minimal `rustow.toml` with `version = 1` and `cache_dir`.
- If `rustow.toml` exists, adds only a missing `version = 1` at the top. Comments and CRLF line ends stay. rustow rejects a malformed file and other versions.
- Appends `/.rustowcache/` to `.gitignore` and `.rustowignore`. It also appends in-repository cache and state directories. It does this only when the rules of that file do not already exclude the directory. If a later negation re-includes the directory, init repairs it once.
- Escapes `\`, `*`, `?` and `[` in these entries with a backslash. For `cache_dir = "c[1]"` the entry is `/c\[1]/`.
- Handles a missing final newline and CRLF.
- Creates the cache directory with mode 0700, or tightens it to 0700.

init does all its checks before it writes. When a check fails, init changes nothing. If a symlink or directory stands in place of one of these files, or something other than a directory is at the cache path, init stops with exit code 3. A configuration error stops it with exit code 2.

A second run writes nothing. It prints "0 changed".

Use `--dry-run` to see the changes first.
