# Troubleshooting

Start with `rustow doctor` and `rustow check`.

## Exit code 3: conflicts

A destination exists and rustow did not create it. Under the default policy rustow changed nothing.

1. Run `rustow diff` or `rustow status`.
2. Move or delete the old file yourself, or run with `--backup`, or `--force`.
3. `--on-conflict skip` deploys the other items.

Backups go to `<state_dir>/backups/<repo-id>/...`.

A "broken symlink" at a destination is a foreign symlink. It is a conflict.

Replacing a regular file with a symlink always needs `--backup` or `--force`.

"cannot unfold the folded directory: X would not be deployed again; run `rustow apply <dir>`": the run is partial. It selects only some sources below a folded link, or an item below it is in conflict or fails. Items below show "the folded directory D cannot be unfolded yet". Fix the other conflicts, then run `rustow apply <dir>`.

"... is not a directory (symlinks are not followed); move it away first" for `cache_dir` or `state_dir`: a symlink or a file is above an in-repository cache or state directory. Every command stops until you move it away.

## MODIFIED

The destination changed since the last apply. For a copy, you edited the file. For a symlink into the cache, you edited the cache entry through the symlink. Edit the source in the repository. Use `--backup` or `--force` to replace the change.

When the source changed too, `status` shows CONFLICT: "source and destination both changed since rustow last applied it". `diff` shows the same line before the content diff. Merge your edit into the source, then use `--backup` or `--force`.

## UNMANAGED

The destination is correct but not recorded. `rustow apply` records it. It writes nothing.

## ORPHANED

The state records a destination whose source is gone. `status` and the deploy commands show it, for example "orphaned ~/.x <- .x (link): source removed; `rustow undeploy .x` removes it". Run the `rustow undeploy` command in the message. `rustow undeploy` with no path removes every managed destination.

## Exit code 4

- "another rustow process": a second run holds the lock. Wait for it.
- Render error: the message gives the template and line. Check for undefined variables. Use `is defined`.
- Decrypt error: see below.
- "cannot write to directory X: Permission denied …; elevated privileges may be required": you cannot write the directory of the destination. Run rustow with the needed rights, or change the directory. Under the default policy nothing was written.
- State file error after the report: rustow wrote the items in the report, but it could not save the state. Fix the state directory, then run `rustow apply` again. It adopts correct destinations.
- Interrupted: rustow stopped between items. Run again.

## Exit code 2: configuration

Read the message. A TOML error gives the line, the column and the key path: `.rustow.toml: line L, column C, key <path>: <message>`. Common causes: missing `version = 1`, an unknown key, a bad mode string, a template `mode` without owner read (0400).

- "holds 'X', which is not rustow's": a configured in-repository `cache_dir` or `state_dir` holds a file that rustow did not write. Move the file, or choose another directory.
- "is reserved by rustow and is never deployed": the path is reserved, for example a name that starts with `.rustow-tmp-`.
- "source '<path>': <reason>": a path argument names a skipped source. The reason is the same as in the warning (see below). Fix the source, or give another path.

## Warnings: skipped sources

A `warning:` line names a source that rustow skipped. The other sources deploy, and the exit code does not change. See [skipped sources](cli.md#skipped-sources).

- "system files deploy only as copies; register it with `rustow file add .rustowroot/etc/x`": a system file is not registered. Run the command in the message. Or add the path to `.rustowignore`. Or leave it: it stays undeployed. See [system files](system-files.md).
- "is a symbolic link; rustow deploys regular files only. Add it to .rustowignore to skip it" (or "is not a regular file"): add the path to `.rustowignore`.
- "nested encryption suffixes are not supported" or "the destination name is empty after removing suffixes": rename the file.
- "has a [[template_rules]] entry but is not a template": remove the rule, or give the file the template extension.
- "templates cannot be registered with `rustow file add`": remove the `[[files]]` entry. Use a `[[template_rules]]` entry to copy a template.
- ".rustow.toml [[files]] entry does not exist" (or "is ignored by .rustowignore", "is reserved", "is a directory"): fix the entry, or remove it with `rustow file remove`.

## Untrusted link target

rustow checks every symlink (see the [trust rule](security.md#trust-rule)). Only home files deploy as symlinks; system files are always copies. The conflict starts with "untrusted link target: repository source: …; copy it instead with `rustow file add`", or "untrusted link target: cache entry: …". A path on the link target chain is not owned by root or by the destination owner, or others can write to it, or a group other than the owner's private group can write to it.

- "writable by group N, which is not the private group of uid M": run the `chmod g-w` command in the message.
- "writable by others": run `chmod o-w` on that path.
- Otherwise deploy the file as a copy: use `rustow file add`, or a template rule for a template.

"symlink target is not safe: …" names a symlinked ancestor of the destination, for example `~/.config`. Its target directory fails the same rule. Fix the permissions of that directory.

## Permission errors for system files

rustow never runs `sudo`. Run rustow with the needed rights. An unreadable destination gives a per-item error with a hint.

## age

- "no configured age identity can decrypt it": add the right identity file to `identity_files`.
- "not valid age ciphertext": the file is damaged or is not age.
- Identity file refused: run `chmod 600` on it. It must be yours, outside the repository and the cache.
- Passphrase-protected identity: use `--interactive`. This is also necessary for `enc edit`.

## GnuPG

- `gpg` not found: install GnuPG 2.2 or newer and put it in `PATH`.
- Recipient refused: use full fingerprints.
- "<fpr> is a subkey fingerprint; configure the primary key fingerprint <primary>": put the primary fingerprint in `recipients`.
- "not trusted": set the key trust in your keyring. rustow does not change it.
- Locked key: unlock `gpg-agent`, or use `--interactive` (also for `enc edit`).
- "not OpenPGP data (empty, damaged or another format)": the `.gpg` file is not OpenPGP. It can be an age file with the wrong suffix.
- "not public-key encrypted OpenPGP data (no recipient key ids)": the file has no recipient. rustow decrypts only data encrypted to public keys.
- "gpg gave no status output; the gpg run failed": gpg did not start or stopped early. Run `rustow doctor` and check the GnuPG home.
- Wrong home: set `[encryption.gpg] homedir` or `$GNUPGHOME`. You must own it. Others must not write to it.

## State file refused

rustow refuses corrupt, foreign or newer state. Move the file `<state_dir>/<repo-id>.json` away. Then run `rustow status`. Existing correct destinations appear as UNMANAGED. `rustow apply` adopts them.

## Stale edit workspace

After an `enc edit` conflict (exit 3), rustow keeps your plaintext in `<cache>/edit/edit-XXXX/`. Copy what you need. Then run `rustow cache clean`.
