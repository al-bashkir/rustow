# Command reference

Run `rustow --help` and `rustow <command> --help` for the full text.

```
rustow [OPTIONS] [COMMAND]
```

Without a command, rustow runs `apply`.

## Global options

| Option | Meaning |
| --- | --- |
| `--repo <DIR>` | Repository root. Default: `$RUSTOW_REPO`, else the nearest ancestor with `.rustow.toml` or `.rustowignore`. Only `init` falls back to the current directory; other commands stop with exit 2. |
| `--target <DIR>` | Target of home sources. |
| `--root-target <DIR>` | Target of `.rustowroot/` sources. |
| `-n`, `--dry-run` | Show what would change. Change nothing. |
| `--backup` | Same as `--on-conflict backup`. |
| `--force` | Same as `--on-conflict overwrite`. |
| `--on-conflict <POLICY>` | `abort` (default), `skip`, `backup`, `overwrite`. |
| `-v`, `--verbose` | Also list unchanged items. |
| `-q`, `--quiet` | Print only conflicts and errors. |
| `--json` | JSON output. |
| `--no-color` | No colors. Also `NO_COLOR`. |
| `--interactive` | Allow passphrase prompts. |
| `-h`, `--help` | Help. |
| `-V`, `--version` | Version. |

## Commands

| Command | Purpose |
| --- | --- |
| `apply [PATH]...` | Deploy all eligible sources. |
| `link [PATH]...` | Deploy ordinary plaintext symlinks only. |
| `unlink [PATH]...` | Remove managed link-mode symlinks (and folded directory links). |
| `init` | Prepare the repository. Deploys nothing. |
| `file add\|remove [PATH]...` | Register or unregister copy sources. |
| `file list` | List registered sources. |
| `file apply` | Deploy registered copies only. |
| `file diff` | Diff registered copies. |
| `template list` | List templates and how they deploy. |
| `template apply` | Deploy templates only (also encrypted). |
| `diff [PATH]...` | Show differences. May print secrets. |
| `status [--ignored] [PATH]...` | Show the state of each destination. |
| `check` | Validate offline. |
| `doctor` | Report on the environment. |
| `undeploy [PATH]...` | Remove managed destinations of any mode. |
| `cache clean` | Remove unused cache entries and stale edit workspaces. |
| `enc [--backend age\|gpg] PATH` | Encrypt a source. |
| `enc edit [--reencrypt] PATH` | Edit an encrypted source. |
| `completions <SHELL>` | Print a completion script (`bash`, `zsh`, `fish`, `elvish`, `powershell`). |

Path arguments are repository-relative. They are never relative to the current directory. A path argument that matches only sources of other modes is a usage error (exit 2).

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success, or clean. |
| 1 | Differences found (`diff`, `status`, `check`). `doctor` uses 1 for a failed check. |
| 2 | Usage or configuration error. |
| 3 | Conflicts. Under the default policy nothing changed. Also when `--on-conflict skip` left conflicts. |
| 4 | Runtime or partial failure. This includes render failure, decrypt failure and interruption. |

## Conflict policies

- `abort` (default): if any item conflicts or fails to plan, rustow writes nothing.
- `skip`: leave conflicting destinations. Deploy the rest. Exit 3.
- `backup`: move the old object to `<state_dir>/backups/<repo-id>/<UTC YYYYmmddTHHMMSSZ>-<pid>/<home|root|cache>/<dest-rel>`. The directories are private.
- `overwrite`: replace the file or symlink.

`backup` and `overwrite` never replace a directory or a special file. They never act through an unsafe ancestor symlink. They never touch a destination that resolves inside the repository or the cache.

## Decisions

| Destination | Action |
| --- | --- |
| Missing | create |
| Own recorded symlink with old target | update |
| Correct but not recorded | adopt (record it; no write). `status` shows UNMANAGED. |
| Foreign file or symlink, not recorded (also a broken symlink) | conflict |
| Managed copy unchanged, desired content changed | update |
| Destination changed since last apply | MODIFIED conflict |

Before it replaces an object, rustow checks the object again. If it changed, the item fails. rustow creates a new destination only if nothing appeared at that path in the meantime. After it writes, rustow verifies. rustow updates the state for each successful item. There is no global rollback. SIGINT, SIGTERM and SIGHUP stop rustow between items. A second signal exits at once.

## status

States: LINKED, COPIED, TEMPLATED (clean); MISSING, OUTDATED, MODIFIED, CONFLICT, UNMANAGED, ERROR (rustow cannot compute the desired state), ORPHANED (recorded, source removed; `undeploy` removes it), IGNORED (with `--ignored`). Clean rows are hidden unless `-v`. Exit 0 when clean, 1 when attention is needed. JSON output never contains content.

## diff

`diff` uses the same plan as `apply`. It prints unified diffs for copies and cache entries. For symlinks and metadata it prints descriptive lines, for example `mode 0600 uid 1000 -> mode 0640`. It summarizes binary files. It can print secrets. Exit 0, 1 or 4.

## check and doctor

`check` is offline. It checks the configuration, ignore rules, registrations and collisions. It renders plaintext templates. It never decrypts. Exit 0, 1 or 2.

`doctor` checks the repository, the configuration, the safety of cache and state directories, target writability, privileges, age identity files, the GnuPG home and `gpg` in `PATH`. It exits with 1 on any failure.

## unlink and undeploy

`unlink` removes link-mode records and folded directory links whose symlink still has the recorded text. `undeploy` removes all modes. It removes a copy only if the content hash is unchanged. Both accept sources that you removed from the repository. If you replaced a folded directory link yourself, they forget its record and leave what is there.

rustow also removes parent directories that it created, only when they are empty.

## cache clean

Removes this repository's cache entries that no recorded symlink uses, and empty directories. It also removes stale `enc edit` workspaces: only directories named `edit-*` that you own, that hold a `.lock` file, and that no running edit uses. It never follows symlinks.
