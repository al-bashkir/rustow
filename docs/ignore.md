# Ignore rules

`<repo>/.rustowignore` lists repository paths that rustow must not deploy. It uses gitignore syntax with Git semantics. An ignored directory cannot have children re-included.

Patterns match repository paths:

| Pattern             | Matches                                    |
| ------------------- | ------------------------------------------ |
| `/rustowroot/etc/x` | The system file `/etc/x`.                  |
| `/etc/`             | The repository directory `etc/` only.      |
| `*.bak`             | Any `.bak` file, also under `rustowroot/`. |

```gitignore
# Private cache.
/.rustowcache/
# Editor backups.
*.bak
# Repository notes.
/README.md
```

To ignore paths on some hosts only, use the `ignore` list of a [`[[global_rules]]`](configuration.md#global_rules) entry.

## Built-in exclusions

rustow always excludes these. A `!` pattern or an explicit path cannot re-include them.

- `.git` at any depth.
- `.gitignore`, `rustow.toml` and `.rustowignore` in the repository root.
- The `.rustowcache/` subtree.
- A cache or state directory that the configuration puts inside the repository. See [cache and state directories](configuration.md#cache-and-state-directories).
- A file named `rustowroot`.
- Any path component that starts with `.rustow-tmp-`, at any depth. rustow names its temporary files `.rustow-tmp-XXXXXX`.

## Special files

rustow skips symlinks and FIFOs in the repository with a warning. Ignore them to remove the warning.

rustow never decrypts an ignored source.

Use `rustow status --ignored` to list ignored paths. Use `rustow check` to validate the rules.
