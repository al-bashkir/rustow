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
| `--interactive` | Allow passphrase prompts: GnuPG pinentry and the age identity prompt. Without it, a locked key fails (exit 4) with a hint. This also applies to `enc edit`. See [encryption](encryption.md#interactive-and-unattended-runs). |
| `-h`, `--help` | Help. |
| `-V`, `--version` | Version. |

## Commands

| Command | Purpose |
| --- | --- |
| `apply [PATH]...` | Deploy all eligible sources. |
| `link [PATH]...` | Deploy ordinary plaintext symlinks only. System files are never symlinks. |
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

Path arguments are repository-relative. They are never relative to the current directory. A path argument that matches only sources of other modes is a usage error (exit 2). A path argument that names a [skipped source](#skipped-sources) is a usage error (exit 2) with the reason, for example "source '.rustowroot/etc/x': system files deploy only as copies; register it with `rustow file add .rustowroot/etc/x`". A directory argument that holds skipped sources deploys the other sources and prints a warning for each skipped one.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success, or clean. |
| 1 | Differences found (`diff`, `status`, `check`). `doctor` uses 1 for a failed check. |
| 2 | Usage or configuration error. A problem with one source is a warning, not exit 2. |
| 3 | Conflicts. Under the default policy nothing changed. Also when `--on-conflict skip` left conflicts. |
| 4 | Runtime or partial failure. This includes render failure, decrypt failure, a destination directory that you cannot write, a state file that rustow cannot write, and interruption. |

## Skipped sources

A problem with one source does not stop the run. rustow skips that source and deploys the other sources. You can leave a file undeployed on purpose. rustow skips:

- an ordinary or encrypted system file that is not in `[[files]]`;
- a name with nested encryption suffixes, such as `x.age.gpg`;
- a name that is empty after rustow removes the suffixes, such as `.age`;
- a file with a `[[template_rules]]` entry that is not a template;
- a template in `[[files]]`;
- a symlink or another file that is not a regular file;
- a `[[files]]` or `[[template_rules]]` entry whose source does not exist, is ignored, is reserved or is a directory.

`apply`, `rustow` without a command, `link`, `file apply`, `template apply` and `diff` print one line on stderr for each skipped source in the path selection:

```text
warning: .rustowroot/etc/x: system files deploy only as copies; register it with `rustow file add .rustowroot/etc/x`
warning: n.age.gpg: nested encryption suffixes are not supported
warning: .vimrc: is a symbolic link; rustow deploys regular files only. Add it to .rustowignore to skip it
warning: gone.json: .rustow.toml [[files]] entry does not exist (remove the entry, e.g. with `rustow file remove`)
```

`-q` hides these lines. With `--json`, the output has a `warnings` array. Each object has `source` and `reason`. Warnings do not change the exit code. `status` shows a skipped source as SKIPPED. `check` prints the warnings, but they are not problems. `unlink` and `undeploy` read only the state, so they do not look at skipped sources.

Problems that are not about one source still stop every command with exit 2. Examples: TOML syntax, an unknown key, an unsupported `version`, a bad `mode` string, a bad cache or state directory, a repository path that is not UTF-8.

## Conflict policies

- `abort` (default): if any item conflicts or fails to plan, rustow writes nothing. The summary then starts with "conflicts found", "errors found" or "conflicts and errors found", then ", nothing written: …".
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
| Symlink in other text that reaches the same source (for example an absolute path) | update (rewrite it as rustow's link text) |
| Foreign file or symlink, not recorded (also a broken symlink) | conflict |
| Managed copy unchanged, desired content changed | update |
| Destination changed since last apply | MODIFIED conflict |
| Cache entry behind rustow's link changed since last apply (also after a switch to a copy) | conflict on the cache entry; `--backup` saves it under `backups/…/cache/<dest>` |
| Several sources map to the destination | conflict (collision) |
| Symlink target fails the [trust rule](security.md#trust-rule) (home files only; system files are copies) | conflict ("untrusted link target") |
| Directory of the destination is not writable | error, also in `--dry-run` and `status` |

Two sources collide when they have the same destination, for example `x`, `x.age` and `x.tmpl`. On a case-insensitive target filesystem, names that differ only in case collide too. A root target of `/` always counts as case-sensitive (known limit).

A destination in a directory that you cannot write is an error already in the plan: "cannot write to directory X: Permission denied …; elevated privileges may be required". So the dry run, `status` (ERROR) and the real run show it. `apply` exits with 4, also with `--dry-run`. Under the default policy, rustow then writes nothing.

Before it replaces an object, rustow checks the object again. If it changed, the item fails. rustow creates a new destination only if nothing appeared at that path in the meantime. After it writes, rustow verifies. rustow updates the state for each successful item. There is no global rollback. SIGINT, SIGTERM and SIGHUP stop rustow between items. A second signal exits at once.

## Deploy output

`apply`, `link`, `file apply`, `template apply`, `unlink` and `undeploy` print one line for each item, then a summary. A dry run summary starts with "dry run, nothing written: ". When the abort policy stops a run, the summary starts with "conflicts found", "errors found" or "conflicts and errors found", then ", nothing written: ".

With `--json` they print one object: `dry_run`, `aborted`, `items` (each with `source`, `dest`, `mode`, `action` and `detail`), `summary` (`changed`, `unchanged`, `adopted`, `conflicts`, `errors`) and `warnings` (each with `source` and `reason`; always empty for `unlink` and `undeploy`). `dry_run` is true only with `-n`. `aborted` is true when the abort policy stopped the run before it wrote anything.

`apply`, `link`, `file apply` and `template apply` also report recorded destinations of their modes whose source is gone, for example "orphaned ~/.x <- .x (link): source removed; `rustow undeploy .x` removes it". In JSON the item has the action `orphaned`. rustow removes nothing, and the exit code does not change.

When rustow cannot write the state file, these commands still print the item report and the summary. Then the state error goes to stderr, and the exit code is 4.

## status

States: LINKED, COPIED, TEMPLATED (clean); MISSING, OUTDATED, MODIFIED, CONFLICT, UNMANAGED, ERROR (rustow cannot compute the desired state, or cannot write the destination directory), ORPHANED (recorded, source removed; the detail is "source removed; `rustow undeploy <source>` removes it"), SKIPPED (a [skipped source](#skipped-sources) in the selection; the detail is the reason), IGNORED (with `--ignored`). Clean rows are hidden unless `-v`. SKIPPED and IGNORED rows always show, and they do not need attention. The summary counts skipped sources apart, for example "1 ok, 1 skipped, 0 need attention". Exit 0 when clean, 1 when attention is needed. JSON output never contains content.

MODIFIED means that the destination, or the cache entry behind rustow's link, changed since rustow last applied it. When the desired content changed too, `status` shows CONFLICT: "source and destination both changed since rustow last applied it".

## diff

`diff` uses the same plan as `apply`. It prints unified diffs for copies and cache entries. For symlinks and metadata it prints descriptive lines, for example `~/.x: metadata mode 0600 uid 1000 gid 1000 -> mode 0640`. It summarizes binary files. It can print secrets. Exit 0, 1 or 4.

When the source and the destination both changed, `diff` prints "<dest>: conflict: source and destination both changed since rustow last applied it" before the content diff. When rustow cannot render or decrypt an item, it prints "rustow: <source>: cannot compute desired content: <message>" to stderr, and the exit code is 4.

`diff --json` and `file diff --json` print `{"items": [...], "warnings": [...]}`, with one object for each item that differs: `dest`, `source`, `mode`, `kind`, `detail`, `binary` and `diff`. `kind` is the status word in lower case (`missing`, `outdated`, `modified`, `conflict`, …), or `metadata` when only the metadata differs. A render or decrypt error has the kind `error`, with the message in `detail`. The `diff` field holds the content, so it can hold secrets. The exit codes are the same: 0, 1 or 4.

## check and doctor

`check` is offline. It checks the configuration, ignore rules, registrations, collisions, and the `owner` and `group` names of template rules. It renders plaintext templates. It never decrypts. Exit 0, 1 or 2. It prints a warning for each [skipped source](#skipped-sources). A warning is not a problem and does not change the exit code. `check --json` prints `sources`, `problems` and `warnings`.

`doctor` checks the repository, the configuration, the safety of cache and state directories, target writability, privileges, age identity files, the GnuPG home and `gpg` in `PATH`. It exits with 1 on any failure.

## unlink and undeploy

`unlink` removes link-mode records and folded directory links whose symlink still has the recorded text. `undeploy` removes all modes. It removes a copy only if the content hash is unchanged. Both accept sources that you removed from the repository. If you replaced a folded directory link yourself, they forget its record and leave what is there.

rustow also removes parent directories that it created, only when they are empty.

## cache clean

Removes this repository's cache entries that no recorded symlink uses, and empty directories. It also removes stale `enc edit` workspaces: only directories named `edit-*` that you own, that hold a `.lock` file, and that no running edit uses. It never follows symlinks.
