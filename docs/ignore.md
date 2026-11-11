# Ignore rules

`<repo>/.rustowignore` lists repository paths that rustow must not deploy. It uses gitignore syntax with Git semantics. An ignored directory cannot have children re-included.

Patterns match repository paths:

| Pattern | Matches |
| --- | --- |
| `/.rustowroot/etc/x` | The system file `/etc/x`. |
| `/etc/` | The repository directory `etc/` only. |
| `*.bak` | Any `.bak` file, also under `.rustowroot/`. |

```gitignore
# Private cache.
/.rustowcache/
# Editor backups.
*.bak
# Repository notes.
/README.md
```

## Built-in exclusions

rustow always excludes these. A `!` pattern or an explicit path cannot re-include them.

- `.git` at any depth.
- `.gitignore`, `.rustow.toml` and `.rustowignore` in the repository root.
- The `.rustowcache/` subtree.
- A cache or state directory that the configuration puts inside the repository.
- A file named `.rustowroot`.

## Special files

Symlinks and FIFOs in the repository are errors. Ignore them.

rustow never decrypts an ignored source.

Use `rustow status --ignored` to list ignored paths. Use `rustow check` to validate the rules.
