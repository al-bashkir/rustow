# Command reference

Run `rustow --help` and `rustow <command> --help` for the full text.

```
rustow [OPTIONS] [COMMAND]
```

Without a command, rustow runs `apply`.

## Global options

| Option                   | Meaning                                                                                                                                                                                                                      |
| ------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `--repo <DIR>`           | Repository root. Default: `$RUSTOW_REPO`, else the nearest ancestor with `rustow.toml` or `.rustowignore`. Only `init` falls back to the current directory; other commands stop with exit 2.                                 |
| `--target <DIR>`         | Target of home sources. A symlink in the path must pass the [trust rule](security.md#trust-rule) for symlinked ancestors. Otherwise, or when the directory is missing, rustow stops with exit 2.                             |
| `--root-target <DIR>`    | Target of `rustowroot/` sources. The same check as for `--target` applies.                                                                                                                                                   |
| `-n`, `--dry-run`        | Show what would change. Change nothing.                                                                                                                                                                                      |
| `--backup`               | Same as `--on-conflict backup`.                                                                                                                                                                                              |
| `--force`                | Same as `--on-conflict overwrite`.                                                                                                                                                                                           |
| `--on-conflict <POLICY>` | `skip` (default), `abort`, `backup`, `overwrite`.                                                                                                                                                                            |
| `-v`, `--verbose`        | Also list unchanged items.                                                                                                                                                                                                   |
| `-q`, `--quiet`          | Print only conflicts and errors.                                                                                                                                                                                             |
| `--json`                 | JSON output. Paths of destinations are absolute; text output shortens the home directory to `~`.                                                                                                                             |
| `--no-color`             | No colors. Also `NO_COLOR` with a value that is not empty.                                                                                                                                                                   |
| `--interactive`          | Allow passphrase prompts: GnuPG pinentry and the age identity prompt. Without it, a locked key fails (exit 4) with a hint. This also applies to `enc edit`. See [encryption](encryption.md#interactive-and-unattended-runs). |
| `-h`, `--help`           | Help.                                                                                                                                                                                                                        |
| `-V`, `--version`        | Version.                                                                                                                                                                                                                     |

## Commands

| Command                         | Purpose                                                                                                                                |
| ------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------- |
| `apply [PATH]...`               | Deploy all eligible sources. In [copy mode](configuration.md#copy-mode), it makes no new symlink.                                      |
| `link [PATH]...`                | Deploy ordinary plaintext symlinks only. System files are never symlinks. In copy mode, only a path argument makes a new link.         |
| `unlink [PATH]...`              | Remove managed link-mode symlinks (and folded directory links), and their copy-mode registrations.                                     |
| `init`                          | Prepare the repository. Deploys nothing.                                                                                               |
| `file add\|remove [PATH]...`    | Register copy sources in the state, or unregister them from the state and `[[files]]`.                                                 |
| `file list`                     | List registered sources.                                                                                                               |
| `file apply`                    | Deploy registered copies only.                                                                                                         |
| `file diff`                     | Diff registered copies.                                                                                                                |
| `template list`                 | List templates and how they deploy.                                                                                                    |
| `template apply`                | Deploy templates only (also encrypted).                                                                                                |
| `diff [PATH]...`                | Show differences. May print secrets.                                                                                                   |
| `status [--ignored] [PATH]...`  | Show the state of each destination.                                                                                                    |
| `check`                         | Validate offline.                                                                                                                      |
| `doctor`                        | Report on the environment.                                                                                                             |
| `undeploy [-y] [PATH]...`       | Remove managed destinations of any mode. Without a path, asks first.                                                                   |
| `cache clean`                   | Remove unused cache entries and stale edit workspaces.                                                                                 |
| `state export [FILE]`           | Print (or write) the `file add` and copy-mode `link` registrations as JSON.                                                            |
| `state import FILE`             | Add the registrations in FILE (`-` for stdin) to this machine.                                                                         |
| `enc [--backend age\|gpg] PATH` | Encrypt a source.                                                                                                                      |
| `enc edit [--reencrypt] PATH`   | Edit an encrypted source.                                                                                                              |
| `completions <SHELL>`           | Print a completion script (`bash`, `zsh`, `fish`, `elvish`, `powershell`). See [shell completions](installation.md#shell-completions). |

Path arguments are repository-relative. They are never relative to the current directory. A path argument that matches only sources of other modes is a usage error (exit 2). A path argument that names a [skipped source](#skipped-sources) is a usage error (exit 2) with the reason, for example "source 'rustowroot/etc/x': system files deploy only as copies; register it with `rustow file add rustowroot/etc/x`". A directory argument that holds skipped sources deploys the other sources and prints a warning for each skipped one.

## Exit codes

| Code | Meaning                                                                                                                                                                            |
| ---- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 0    | Success, or clean.                                                                                                                                                                 |
| 1    | Differences found (`diff`, `status`, `check`). `doctor` uses 1 for a failed check.                                                                                                 |
| 2    | Usage or configuration error. A problem with one source is a warning, not exit 2.                                                                                                  |
| 3    | Conflicts. The default policy deployed the other items. Under `--on-conflict abort` nothing changed.                                                                               |
| 4    | Runtime or partial failure. This includes render failure, decrypt failure, a destination directory that you cannot write, a state file that rustow cannot write, and interruption. |

## Skipped sources

A problem with one source does not stop the run. rustow skips that source and deploys the other sources. You can leave a file undeployed on purpose. rustow skips:

- an ordinary or encrypted system file that is not in `[[files]]`;
- a name with nested encryption suffixes, such as `x.age.gpg`;
- a name that is empty after rustow removes the suffixes, such as `.age`;
- a file with a `[[template_rules]]` entry that is not a template;
- a template in `[[files]]`;
- a symlink or another file that is not a regular file;
- a directory or a path below the repository root that rustow cannot read, for example a directory without read permission or a path that is too long ("cannot read: Permission denied …"). A directory above it does not fold. A repository root that rustow cannot read still stops the command;
- a `[[files]]` or `[[template_rules]]` entry, or a `rustow file add` registration, whose source does not exist, is ignored, is reserved or is a directory.

`apply`, `rustow` without a command, `link`, `file apply`, `template apply` and `diff` print one line on stderr for each skipped source in the path selection:

```text
warning: rustowroot/etc/x: system files deploy only as copies; register it with `rustow file add rustowroot/etc/x`
warning: n.age.gpg: nested encryption suffixes are not supported
warning: .vimrc: is a symbolic link; rustow deploys regular files only. Add it to .rustowignore to skip it
warning: gone.json: rustow.toml [[files]] entry does not exist (remove the entry, e.g. with `rustow file remove`)
```

`-q` hides these lines. With `--json`, the output has a `warnings` array. Each object has `source` and `reason`. Warnings do not change the exit code. `status` shows a skipped source as SKIPPED. `check` prints the warnings, but they are not problems. `unlink` and `undeploy` read only the state, so they do not look at skipped sources.

Problems that are not about one source still stop every command with exit 2. Examples: TOML syntax, an unknown key, an unsupported `version`, a bad `mode` string, a repository path that is not UTF-8, a cache or state directory that is the repository or one of its parents, or an in-repository cache or state directory that holds files that are not rustow's. Some problems with these directories have other exit codes: a state directory that you do not own, or that is not a real directory, exits 4. A symlink or a non-directory above an in-repository cache or state directory exits 3. See [cache and state directories](configuration.md#cache-and-state-directories).

## Conflict policies

- `skip` (default): leave conflicting destinations. Deploy the rest. An item that fails to plan (for example a render error) does not stop the other items. Exit 3, or 4 for an error.
- `abort`: if any item conflicts or fails to plan, rustow writes nothing. The summary then starts with "conflicts found", "errors found" or "conflicts and errors found", then ", nothing written: …".
- `backup`: move the old object to `<state_dir>/backups/<repo-id>/<UTC YYYYmmddTHHMMSSZ>-<pid>/<home|root|cache>/<dest-rel>`. The directories are private.
- `overwrite`: replace the file or symlink.

`backup` and `overwrite` never replace a directory or a special file. They never act through an unsafe ancestor symlink. They never touch a destination that resolves inside the repository or the cache.

## Decisions

| Destination                                                                                              | Action                                                                          |
| -------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------- |
| Missing                                                                                                  | create                                                                          |
| Own recorded symlink with old target                                                                     | update                                                                          |
| Correct but not recorded                                                                                 | adopt (record it; no write). `status` shows UNMANAGED.                          |
| Symlink in other text that reaches the same source (for example an absolute path)                        | update (rewrite it as rustow's link text)                                       |
| Foreign file or symlink, not recorded (also a broken symlink)                                            | conflict                                                                        |
| Managed copy unchanged, desired content changed                                                          | update                                                                          |
| Destination changed since last apply                                                                     | MODIFIED conflict                                                               |
| Cache entry behind rustow's link changed since last apply (also after a switch to a copy)                | conflict on the cache entry; `--backup` saves it under `backups/…/cache/<dest>` |
| Several sources map to the destination                                                                   | conflict (collision)                                                            |
| Symlink target fails the [trust rule](security.md#trust-rule) (home files only; system files are copies) | conflict ("untrusted link target")                                              |
| Directory of the destination is not writable                                                             | error, also in `--dry-run` and `status`; `diff` still shows the content         |

Two sources collide when they have the same destination, for example `x`, `x.age` and `x.tmpl`. On a case-insensitive target filesystem, names that differ only in case collide too. A root target of `/` always counts as case-sensitive (known limit).

A home destination in a directory that you cannot write is an error already in the plan: "cannot write to directory X: Permission denied …; elevated privileges may be required". So the dry run, `status` (ERROR) and the real run show it. `apply` exits with 4, also with `--dry-run`. Under the default policy, rustow writes the other items and reports this item as an error. With `--on-conflict abort`, rustow writes nothing. `diff` only reads, so it still shows the desired content of such an item. A system file that you cannot write goes through sudo instead ("via sudo"). See [system files](system-files.md#privileges).

Before it replaces an object, rustow checks the object again. If it changed, the item fails. rustow creates a new destination only if nothing appeared at that path in the meantime. After it writes, rustow verifies. There is no global rollback. SIGINT, SIGTERM and SIGHUP stop rustow between items. A second signal exits at once. rustow saves the state file once, at the end of the run. It also saves it when a first signal stops the run. After SIGKILL or a second signal, rustow saves no record of this run. Then the next `apply` adopts the destinations that are correct.

## Deploy output

`apply`, `link`, `file apply`, `template apply`, `unlink` and `undeploy` print one line for each item, then a summary. Conflicts and errors are not in the item list. They come after it, below a "Conflicts:" line, before the summary. `-q` prints only this block. A dry run summary starts with "dry run, nothing written: ". When the abort policy stops a run, the summary starts with "conflicts found", "errors found" or "conflicts and errors found", then ", nothing written: ".

Human output escapes control characters in names and messages, on stdout and stderr: a newline shows as `\n`, a tab as `\t`, a carriage return as `\r`, and any other control character (for example ESC) as `\xNN`. So a file name cannot make a line that looks like a report line. JSON output keeps the names as they are; JSON escapes them itself.

With `--json` they print one object: `dry_run`, `aborted`, `items` (each with `source`, `dest`, `mode`, `action` and `detail`), `summary` (`changed`, `unchanged`, `adopted`, `conflicts`, `errors`) and `warnings` (each with `source` and `reason`; always empty for `unlink` and `undeploy`). `dry_run` is true only with `-n`. `aborted` is true when the abort policy stopped the run before it wrote anything.

`apply`, `link`, `file apply` and `template apply` also report recorded destinations of their modes whose source is gone, for example "orphaned ~/.x <- .x (link): source removed; `rustow undeploy .x` removes it". In JSON the item has the action `orphaned`. rustow removes nothing, and the exit code does not change.

When rustow cannot write the state file, these commands still print the item report and the summary. Then the state error goes to stderr, and the exit code is 4.

## status

States: LINKED, COPIED, TEMPLATED (clean); MISSING, OUTDATED, MODIFIED, CONFLICT, UNMANAGED, ERROR (rustow cannot compute the desired state, or cannot write the destination directory), ORPHANED (recorded, source removed; the detail is "source removed; `rustow undeploy <source>` removes it". For a template whose [meta condition](templates.md#meta-block) became false, the detail names the condition instead. A copy whose `[[files]]` entry or `file add` registration names a removed source is ORPHANED too, and its SKIPPED row stays), SKIPPED (a [skipped source](#skipped-sources) in the selection, or a template whose meta condition is false on this machine; the detail is the reason), IGNORED (with `--ignored`). Clean rows are hidden unless `-v`. SKIPPED and IGNORED rows always show, and they do not need attention. The summary counts skipped sources and ignored paths apart, for example "1 ok, 1 skipped, 0 need attention" or "1 ok, 1 ignored, 0 need attention". Exit 0 when clean, 1 when attention is needed. JSON output never contains content.

MODIFIED means that the destination, or the cache entry behind rustow's link, changed since rustow last applied it. When the desired content changed too, `status` shows CONFLICT: "source and destination both changed since rustow last applied it".

## diff

`diff` uses the same plan as `apply`. It prints unified diffs for copies and cache entries. For symlinks and metadata it prints descriptive lines, for example `~/.x: metadata mode 0600 uid 1000 gid 1000 -> mode 0640`. It summarizes binary files. It can print secrets. Exit 0, 1 or 4.

When the source and the destination both changed, `diff` prints "<dest>: conflict: source and destination both changed since rustow last applied it" before the content diff. When rustow cannot render or decrypt an item, it prints "rustow: <source>: cannot compute desired content: <message>" to stderr, and the exit code is 4.

`diff --json` and `file diff --json` print `{"items": [...], "warnings": [...]}`, with one object for each item that differs: `dest`, `source`, `mode`, `kind`, `detail`, `binary` and `diff`. `kind` is the status word in lower case (`missing`, `outdated`, `modified`, `conflict`, …), or `metadata` when only the metadata differs. A render or decrypt error has the kind `error`, with the message in `detail`. The `diff` field holds the content, so it can hold secrets. The exit codes are the same: 0, 1 or 4.

## check and doctor

`check` is offline. It checks the configuration, ignore rules, registrations, collisions, and the `owner` and `group` names of template rules. It renders plaintext templates. It never decrypts. Exit 0, 1 or 2. It prints a warning for each [skipped source](#skipped-sources). A warning is not a problem and does not change the exit code. Exception: a mistake in a template [meta block](templates.md#meta-block) is a problem. `check --json` prints `sources`, `problems` and `warnings`.

`doctor` checks the repository, the configuration, the safety of cache and state directories, target writability, privileges, age identity files, the GnuPG home and `gpg` in `PATH`. It exits with 1 on any failure.

## unlink and undeploy

`unlink` removes link-mode records and folded directory links whose symlink still has the recorded text. `undeploy` removes all modes. It removes a copy only if the content hash is unchanged. Both accept sources that you removed from the repository. If you replaced a folded directory link yourself, they forget its record and leave what is there.

rustow also removes parent directories that it created, only when they are empty.

`undeploy` without a path removes every managed destination. So a real run asks first. It prints the list on stderr, then "Remove N destinations? [y/N]". Only `y` or `yes` continues. Any other answer prints "cancelled, nothing removed" and exits 0. Use `--yes` (`-y`) to skip the question. Without a terminal on stdin and without `--yes`, rustow removes nothing and exits 2. rustow does not ask for `--dry-run`, for a path argument, for `unlink`, or when nothing would be removed.

## cache clean

Removes this repository's cache entries that no recorded symlink uses, and empty directories. It also removes stale `enc edit` workspaces: only directories named `edit-*` that you own, that hold a `.lock` file, and that no running edit uses. It never follows symlinks.

## state export and import

The state file of one machine holds registrations: `file add` copies and copy-mode `link` links. To use them on another machine:

```sh
rustow state export regs.json      # on the old machine (no FILE: print to stdout)
rustow state import regs.json      # on the new machine ("-" reads stdin)
```

The export is JSON: `{"version": 1, "files": [...], "links": [...]}`. It holds repository-relative paths only. The records of deployed files stay out: they belong to one machine. `import` adds each registration that is not there yet and prints it. It removes nothing. It checks each `files` path as `file add` does, and each `links` path as `rustow link <path>` does: the path must name one file that can deploy that way. It rejects a bad path, an unknown version or an unknown key with exit 2. The message names the path and the reason. Then it changes nothing. `import` reads at most 512 MiB. A larger input, or a FILE that is not a regular file (a directory, a FIFO, a device such as `/dev/zero`), is refused with exit 2. The state file has the same limit: a state file that is larger, or that is not a regular file, stops every command with exit 2. `import -n` shows what it would add. `export` has no `--dry-run`.
