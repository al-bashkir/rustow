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
| `--backup`               | Same as `--on-conflict backup`. `unlink` and `undeploy` refuse it.                                                                                                                                                           |
| `--force`                | Same as `--on-conflict overwrite`. `unlink` and `undeploy` refuse it (`state export` uses it to replace FILE).                                                                                                               |
| `--on-conflict <POLICY>` | `skip` (default), `abort`, `backup`, `overwrite`. `unlink` and `undeploy` take only `skip` and `abort`.                                                                                                                      |
| `-v`, `--verbose`        | Also list unchanged items.                                                                                                                                                                                                   |
| `-q`, `--quiet`          | Print only conflicts and errors.                                                                                                                                                                                             |
| `--json`                 | JSON output. Paths of destinations are absolute; text output shortens the home directory to `~`.                                                                                                                             |
| `--no-color`             | No colors. Also `NO_COLOR` with a value that is not empty.                                                                                                                                                                   |
| `--interactive`          | Allow passphrase prompts: GnuPG pinentry and the age identity prompt. Without it, a locked key fails (exit 4) with a hint. This also applies to `enc edit`. See [encryption](encryption.md#interactive-and-unattended-runs). |
| `-h`, `--help`           | Help.                                                                                                                                                                                                                        |
| `-V`, `--version`        | Version.                                                                                                                                                                                                                     |

A relative path in `--repo`, `RUSTOW_REPO`, `--target` or `--root-target` starts at the working directory. rustow needs the working directory only for such a path and to find the repository. When the working directory was deleted, an absolute path still works. Else rustow stops with exit 4: "current directory: No such file or directory".

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
| `state export [FILE]`           | Print (or write) the `file add` and copy-mode `link` registrations as JSON. `-` prints them.                                           |
| `state import FILE`             | Add the registrations in FILE (`-` for stdin) to this machine.                                                                         |
| `state adopt OLD_ROOT`          | Take over the state of this repository from its path before a move.                                                                    |
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
| 130  | A second SIGINT, SIGTERM or SIGHUP while rustow changes files. rustow exits at once.                                                                                               |
| 141  | The reader of the output closed the pipe, for example `rustow status \| head`.                                                                                                     |

rustow catches SIGINT, SIGTERM and SIGHUP only while it changes files: in `apply`, `link`, `file apply`, `template apply`, `unlink` and `undeploy` after the plan, and in `enc edit`. The first signal stops the run between items (exit 4). A second signal exits at once with 130. At any other time, for example while rustow plans or in a command that only reads, a signal ends rustow with the default action of the signal. The shell then shows 128 plus the number of the signal: 130 for SIGINT, 129 for SIGHUP and 143 for SIGTERM.

## Skipped sources

A problem with one source does not stop the run. rustow skips that source and deploys the other sources. You can leave a file undeployed on purpose. rustow skips:

- an ordinary or encrypted system file that is not in `[[files]]`;
- a name with nested encryption suffixes, such as `x.age.gpg`;
- a name that is empty after rustow removes the suffixes, such as `.age`;
- a file with a `[[template_rules]]` entry that is not a template;
- a template in `[[files]]`;
- a symlink or another file that is not a regular file;
- a file or directory whose name is not valid UTF-8 ("name is not valid UTF-8; rename it"). The warning shows each bad byte sequence as `�`. rustow skips everything below such a directory, and a directory above it does not fold. A `.rustowignore` pattern that matches the name (with `�`) ignores it without a warning;
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

Problems that are not about one source still stop every command with exit 2. Examples: a `rustow.toml` or `.rustowignore` that rustow cannot read (the message names the file and the cause, for example "Permission denied"), TOML syntax, an unknown key, an unsupported `version`, a bad `mode` string, a path to the repository root that is not UTF-8, a cache or state directory that is the repository or one of its parents, or an in-repository cache or state directory that holds files that are not rustow's. Some problems with these directories have other exit codes: a state directory that you do not own, or that is not a real directory (also a symlink to a directory), exits 4. A symlink above the state directory is followed. A symlink or a non-directory above an in-repository cache or state directory exits 3. See [cache and state directories](configuration.md#cache-and-state-directories).

## Conflict policies

- `skip` (default): leave conflicting destinations. Deploy the rest. An item that fails to plan (for example a render error) does not stop the other items. Exit 3, or 4 for an error.
- `abort`: if any item conflicts or fails to plan, rustow writes nothing. The summary then starts with "conflicts found", "errors found" or "conflicts and errors found", then ", nothing written: …".
- `backup`: move the old object to `<state_dir>/backups/<repo-id>/<UTC YYYYmmddTHHMMSSZ>-<pid>/<home|root|cache>/<dest-rel>`. The directories are private. rustow makes them as you before it changes the item, also for a system file that the sudo helper replaces. It makes them only for an item whose change can move an object away: a conflict, or a change of a file that you cannot read or see. Such a file can still hold the recorded content: then nothing moves. When the state directory is on another filesystem, rustow copies the object into a temporary file there, renames it to the backup name, and then removes the original. When the copy fails (for example, the disk is full), no partial backup stays, the original stays in place, and the item fails. At the end of the run, rustow removes each backup directory of the run that stays empty.
- `overwrite`: replace the file or symlink.

`backup` and `overwrite` never replace a directory or a special file. They never act through an unsafe ancestor symlink. They never touch a destination that resolves inside the repository, the cache or the state directory: such an item is a conflict.

## Decisions

| Destination                                                                                              | Action                                                                                                          |
| -------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------- |
| Missing                                                                                                  | create                                                                                                          |
| Own recorded symlink with old target                                                                     | update                                                                                                          |
| Correct but not recorded                                                                                 | adopt (record it; no write). `status` shows UNMANAGED.                                                          |
| Symlink in other text that reaches the same source (for example an absolute path)                        | update (rewrite it as rustow's link text)                                                                       |
| Foreign file or symlink, not recorded (also a broken symlink)                                            | conflict                                                                                                        |
| Managed copy unchanged, desired content changed                                                          | update                                                                                                          |
| Destination changed since last apply                                                                     | MODIFIED conflict                                                                                               |
| Cache entry behind rustow's link changed since last apply (also after a switch to a copy)                | conflict on the cache entry; `--backup` saves it under `backups/…/cache/<dest>`                                 |
| Several sources map to the destination                                                                   | conflict (collision)                                                                                            |
| Symlink target fails the [trust rule](security.md#trust-rule) (home files only; system files are copies) | conflict ("untrusted link target")                                                                              |
| Directory of the destination is not writable                                                             | error, also in `--dry-run` and `status`; `diff` still shows the content                                         |
| File that you cannot read, recorded copy                                                                 | compared with the state; see [system files](system-files.md#destinations-that-you-cannot-read)                  |
| File that you cannot read, not recorded                                                                  | conflict ("cannot read the destination; rustow has no record of it")                                            |
| Destination in a directory that you cannot search                                                        | compared with the state; see [system files](system-files.md#destinations-in-a-directory-that-you-cannot-search) |

Two sources collide when they have the same destination, for example `x`, `x.age` and `x.tmpl`. On a case-insensitive target filesystem, names that differ only in case collide too. A root target of `/` always counts as case-sensitive (known limit).

A home destination in a directory that you cannot write is an error already in the plan: "cannot write to directory X: Permission denied …; elevated privileges may be required". So the dry run, `status` (ERROR) and the real run show it. `apply` exits with 4, also with `--dry-run`. Under the default policy, rustow writes the other items and reports this item as an error. With `--on-conflict abort`, rustow writes nothing. `diff` only reads, so it still shows the desired content of such an item. A system file that you cannot write goes through sudo instead ("via sudo"). See [system files](system-files.md#privileges).

Before it replaces an object, rustow checks the object again. If it changed, the item fails. A change of only the mode or the owner checks the file that it opened and changes that same file, so a file that takes the name in the meantime is never changed. rustow creates a new destination only if nothing appeared at that path in the meantime. After it writes, rustow verifies. A failed item removes the empty directories that it created, deepest first; the state records only the created directories that stay. There is no global rollback. SIGINT, SIGTERM and SIGHUP stop rustow between items. This holds from the moment rustow takes the lock, also while it still plans: then it changes nothing, each item fails with "interrupted before this item; nothing changed", and rustow exits with 4. A second signal exits at once. Two cases end rustow at once with the exit code of the signal (130 for SIGINT, 143 for SIGTERM, 129 for SIGHUP), and nothing changed: a signal at the question of `undeploy`, and a signal to a command that changes no destination (for example `status`, or `apply` before it takes the lock). rustow saves the state file once, at the end of the run. It also saves it when a first signal stops the run. After SIGKILL or a second signal, rustow saves no record of this run. Then the next `apply` adopts the destinations that are correct.

## Deploy output

`apply`, `link`, `file apply`, `template apply`, `unlink` and `undeploy` print one line for each item, then a summary. Conflicts and errors are not in the item list. They come after it, below a "Conflicts:" line, before the summary. `-q` prints only this block. The items that a first signal kept from running share one line in this block, for example "error interrupted before 2905 items; nothing changed". JSON gives each of them, with the detail "interrupted before this item; nothing changed". A dry run summary starts with "dry run, nothing written: ". When the abort policy stops a run, the summary starts with "conflicts found", "errors found" or "conflicts and errors found", then ", nothing written: ".

Human output escapes control characters in names and messages, on stdout and stderr: a newline shows as `\n`, a tab as `\t`, a carriage return as `\r`, and any other control character (for example ESC) as `\xNN`. A Unicode bidirectional control (U+061C, U+200E, U+200F, U+202A to U+202E, U+2066 to U+2069) shows as `\u{NNNN}`, for example `\u{202e}`. A backslash shows as `\\`, so the name `tab\there` and a name with a real tab look different. So a file name cannot make a line that looks like a report line, or look like another name. JSON output keeps the names as they are; JSON escapes them itself.

With `--json` they print one object: `dry_run`, `aborted`, `items` (each with `source`, `dest`, `mode`, `action` and `detail`), `summary` (`changed`, `unchanged`, `adopted`, `conflicts`, `errors`) and `warnings` (each with `source` and `reason`; for `unlink` and `undeploy`, only the records under another target, see [status](#status)). `dry_run` is true only with `-n`. `aborted` is true when the abort policy stopped the run before it wrote anything.

`apply`, `link`, `file apply` and `template apply` also report recorded destinations of their modes whose source is gone, for example "orphaned ~/.x <- .x (link): source removed; `rustow undeploy .x` removes it". In JSON the item has the action `orphaned`. rustow removes nothing, and the exit code does not change.

When rustow cannot write the state file, these commands still print the item report and the summary. Then the state error goes to stderr, and the exit code is 4.

## status

States: LINKED, COPIED, TEMPLATED (clean); MISSING, OUTDATED, MODIFIED, CONFLICT, UNMANAGED, ERROR (rustow cannot compute the desired state, or cannot write the destination directory), ORPHANED (recorded, source removed; the detail is "source removed; `rustow undeploy <source>` removes it". For a template whose [meta condition](templates.md#meta-block) became false, the detail names the condition instead. When the path is still there but is not a source, the detail says why: "source is ignored by .rustowignore", "source is reserved and can never be deployed" or "source is a directory, not a file". A copy whose `[[files]]` entry or `file add` registration names such a path is ORPHANED too, and its SKIPPED row stays), SKIPPED (a [skipped source](#skipped-sources) in the selection, or a template whose meta condition is false on this machine; the detail is the reason. A recorded destination whose source is below an unreadable directory is SKIPPED too, with the reason of the directory: rustow cannot tell if the source is still there), IGNORED (with `--ignored`; a path argument selects the ignored paths at or below it, and the ignored directory above it. With `--ignored`, a path argument may name an ignored path; without it, that is a usage error). Clean rows are hidden unless `-v`. SKIPPED and IGNORED rows always show, and they do not need attention. The summary counts skipped sources and ignored paths apart, for example "1 ok, 1 skipped, 0 need attention" or "1 ok, 1 ignored, 0 need attention". Exit 0 when clean, 1 when attention is needed. JSON output never contains content.

MODIFIED means that the destination, or the cache entry behind rustow's link, changed since rustow last applied it. When the desired content changed too, `status` shows CONFLICT: "source and destination both changed since rustow last applied it". Another CONFLICT row has the detail that `apply` gives, for example "existing destination is not managed by rustow (broken symlink to gone)", or "cache entry: …" for a conflict of the cache entry.

The state can record a destination that is not under the current target, for example after you change `defaults.target` or use `--target` once. `status`, `unlink` and `undeploy` do not show or remove it. They print a warning for each such record, with the command that removes it: "warning: .bashrc: deployed to /home/me/.bashrc, which is not under the current target; `rustow --target /home/me undeploy .bashrc` removes it". For a system file, the command has `--root-target`. The warnings do not change the exit code. With `--json`, `status` puts them in a `warnings` array.

## diff

`diff` uses the same plan as `apply`. It prints unified diffs for copies and cache entries. For symlinks and metadata it prints descriptive lines, for example `~/.x: metadata mode 0600 uid 1000 gid 1000 -> mode 0640`. It summarizes binary files. A missing file whose wanted content is empty shows only the two header lines, `--- /dev/null` and `+++ b/<dest>`. It can print secrets. Exit 0, 1 or 4.

`diff` reads the content of a file only when its hash differs from the wanted one. It shows content up to 1 MiB. When the current file or the wanted content is larger, it prints one line with both sizes instead, for example "~/.big: differ (0 → 2097152 bytes); too large to show". A missing file counts as 0 bytes.

When the source and the destination both changed, `diff` prints "<dest>: conflict: source and destination both changed since rustow last applied it" before the content diff. When rustow cannot render or decrypt an item, it prints "rustow: <source>: cannot compute desired content: <message>" to stderr, and the exit code is 4. For a destination that you cannot read and that would change, it prints "rustow: <dest>: cannot read the destination (Permission denied); elevated privileges may be required" to stderr and goes on with the other items; the exit code is 4.

`diff --json` and `file diff --json` print `{"items": [...], "warnings": [...]}`, with one object for each item that differs: `dest`, `source`, `mode`, `kind`, `detail`, `binary` and `diff`. `kind` is the status word in lower case (`missing`, `outdated`, `modified`, `conflict`, …), or `metadata` when only the metadata differs. A render or decrypt error has the kind `error`, with the message in `detail`. The text in `diff` names the destination by its absolute path, as `dest` does (`--- a//home/me/.x`). The `diff` field holds the content, so it can hold secrets. The exit codes are the same: 0, 1 or 4.

## check and doctor

`check` is offline. It checks the configuration, ignore rules, registrations, collisions, the `owner` and `group` names of template rules, and the [trust](security.md#trust-rule) of the sources of system files. It renders plaintext templates. It never decrypts. Exit 0, 1 or 2. It prints a warning for each [skipped source](#skipped-sources). A warning is not a problem and does not change the exit code. Exception: a mistake in a template [meta block](templates.md#meta-block) is a problem. `check --json` prints `sources`, `problems` and `warnings`.

`doctor` checks the repository, the configuration, the safety of cache and state directories, target writability, privileges, the owner of the backup directories, temporary files that an interrupted run left, age identity files, the GnuPG home and `gpg` in `PATH`. It exits with 1 on any failure.

- Each directory under `<state_dir>/backups` that another user owns gets a "backups" warning: a backup into it fails. The warning gives the command that gives it back to you, for example "give it back with `sudo chown -R 1000 -- <path>`".

## Temporary files of an interrupted run

rustow writes each file through a temporary file (`.rustow-tmp-…`) in the same directory. While a command holds the repository lock, rustow first adds the path of each temporary file to `<state_dir>/<repo-id>.pending`, and syncs it. A run that ends normally has no temporary file left and removes that journal. A run that dies (killed, power loss) can leave temporary files. rustow never deploys them: their names are reserved.

- The next command that changes files (it takes the lock) removes each leftover that the journal lists, and prints "note: removed <path>: a temporary file that an interrupted run left".
- A leftover that you cannot remove (for example a file of the sudo helper in `/etc`) stays, and so does its line in the journal. rustow prints the command that removes it: "<path>: a temporary file that an interrupted run left; remove it with `sudo rm -- <path>`". In the command, the shell reads the path back to the same bytes. A path with only letters, digits and `/._-+:@%,=` stays as it is. Another path is in single quotes. A path with a character that human output escapes (a control character, a bidirectional control or a backslash) is in `$'...'` (bash, zsh), with `\xHH` for each byte that is not printable ASCII, for example `sudo rm -- $'/etc/a\x1bb/.rustow-tmp-…'`. rustow never runs sudo only to clean up.
- rustow looks at a leftover only through its directory, walked with the [trust rule](security.md#trust-rule). When that walk is blocked (for example, a symlink on the way that another user owns, or a directory that changed), rustow does not look and gives no command: "<path>: cannot look for a temporary file that an interrupted run left: <dir>: <reason>". The line stays in the journal.
- The sudo helper renames or removes the temporary file of a job before it answers. The helper first prints a line that says that it runs, and rustow sends no job before that line. So rustow drops the journal line of each job that the helper answered, and of each job that rustow did not send: also every job when sudo stopped before the helper started (for example, after a wrong password). A job that rustow sent to the running helper and that got no answer keeps its line, because rustow cannot tell if the helper started it (for example, the helper was killed).
- Such a leftover can be in a directory that you cannot search. Then rustow cannot see that it is gone. After you remove it, remove `<state_dir>/<repo-id>.pending` too.
- `doctor` shows a "temp file" warning for each leftover, with the same advice.
- When rustow cannot start the journal (for example, the state directory is read-only), it prints "warning: …; the temporary files of this run are not journaled" and goes on.
- `init` and `state export` do not take the lock, so they journal nothing. `enc` keeps the plaintext under a temporary name while it checks it; that file is not journaled, because it can be your only copy.

## unlink and undeploy

`unlink` removes link-mode records and folded directory links whose symlink still has the recorded text. `undeploy` removes all modes. It removes a copy only if the content hash is unchanged. With a template or decrypted link, it also removes the cache entry behind it when that is unchanged; an entry that you edited through the link stays, and the next `apply` reports it. A system file that you cannot read goes through the sudo helper, which checks the hash as root. An item that fails does not stop the others. Both accept sources that you removed from the repository. If you replaced a folded directory link yourself, they forget its record and leave what is there. They do the same when a directory above a destination now resolves inside the repository or its cache (for example, you replaced it with a symlink into the repository): what is there is a source, so they never remove it. They also forget the record of a destination that is gone. In all these cases the item line is "forget <dest> <- <source> (<mode>): gone or replaced; record forgotten, anything there left in place".

rustow also removes parent directories that it created, only when they are empty. When you remove such a directory yourself, the next `apply`, `link`, `file apply`, `template apply`, `unlink` or `undeploy` forgets it. A directory that you make there later is yours.

A destination that changed stays in place, and the item is a conflict. Under the default policy (`skip`), rustow removes the other items. Under `--on-conflict abort`, it removes nothing when any item is a conflict or an error. `unlink` and `undeploy` do not take `--backup`, `--force`, `--on-conflict backup` or `--on-conflict overwrite`: they exit 2. Move or remove a changed destination yourself.

`undeploy` without a path removes every managed destination. So a real run asks first. It prints the list on stderr, then "Remove N destinations? [y/N]". Only `y` or `yes` continues. Upper or lower case and spaces before or after the word are accepted, for example `Y` or ` YES`. Any other answer prints "cancelled, nothing removed" and exits 0. Use `--yes` (`-y`) to skip the question. Without a terminal on stdin and without `--yes`, rustow removes nothing and exits 2. rustow does not ask for `--dry-run`, for a path argument, for `unlink`, or when nothing would be removed.

## cache clean

Removes this repository's cache entries that no recorded symlink uses, and empty directories. It also removes stale `enc edit` workspaces: only directories named `edit-*` that you own, that hold a `.lock` file, and that no running edit uses. It never follows symlinks.

## state export and import

The state file of one machine holds registrations: `file add` copies and copy-mode `link` links. To use them on another machine:

```sh
rustow state export regs.json      # on the old machine (no FILE or "-": print to stdout)
rustow state import regs.json      # on the new machine ("-" reads stdin)
```

The export is JSON: `{"version": 1, "files": [...], "links": [...]}`. It holds repository-relative paths only. The records of deployed files stay out: they belong to one machine. `import` adds each registration that is not there yet and prints it. It removes nothing. A file has one registration: a path in both lists imports as a copy, and an imported copy replaces a link registration of the same path. A link of a registered copy exits 2, as `rustow link <path>` does. It checks each `files` path as `file add` does, and each `links` path as `rustow link <path>` does: the path must name one file that can deploy that way. It rejects a bad path, an unknown version or an unknown key with exit 2. The message names the path and the reason. Then it changes nothing. `import` reads at most 512 MiB. A larger input, or a FILE that is not a regular file (a directory, a FIFO, a device such as `/dev/zero`), is refused with exit 2. The state file has the same limit: a state file that is larger, or that is not a regular file (also a symlink, which rustow never follows there), stops every command with exit 2. `import -n` shows what it would add. `export` has no `--dry-run`. `export FILE` makes a new file. When FILE exists, it exits 2 and changes nothing; add `--force` to replace FILE.

## state adopt

The state file belongs to the repository path. After you move the repository (`mv ~/dotfiles ~/dotfiles2`), rustow finds no state for the new path. While this repository has no state file, and rustow finds a state file whose recorded repository path does not exist any more, each command prints a notice:

```
note: the state of a moved repository was found: /home/me/dotfiles; run `rustow state adopt /home/me/dotfiles` to take it over
```

The notice stops when a command saves a state for the new path, for example the first `apply` there. `adopt` then refuses, because this repository has a state. To adopt the old state after that, move the new state file away first.

`rustow state adopt OLD_ROOT` takes the lock. It reads the state of OLD_ROOT with the same checks as every command. It sets the repository path to this repository, and moves the cache entries of the old path to the cache entries of this repository. It saves the result as the state of this repository. It keeps the old file as `<state_dir>/backups/<old-repo-id>/state-adopted.json`. It removes the old lock file `<state_dir>/<old-repo-id>.lock`, unless another run holds it. Then run `rustow apply`: it updates rustow's links to the new path. Copies and registrations stay. `adopt -n` shows what it would do.

`adopt` refuses with exit 2 when OLD_ROOT still exists, when there is no state for OLD_ROOT, or when this repository already has a state file (move it away first). A corrupt, foreign or newer old state exits 4, as for every command. The notice also shows for the state of a repository that you removed: move that file away from the state directory.
