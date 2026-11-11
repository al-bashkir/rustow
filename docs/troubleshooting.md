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

## MODIFIED

The destination changed since the last apply. For a copy, you edited the file. For a symlink into the cache, you edited the cache entry through the symlink. Edit the source in the repository. Use `--backup` or `--force` to replace the change.

## UNMANAGED

The destination is correct but not recorded. `rustow apply` records it. It writes nothing.

## ORPHANED

The state records a destination whose source is gone. Run `rustow undeploy`.

## Exit code 4

- "another rustow process": a second run holds the lock. Wait for it.
- Render error: the message gives the template and line. Check for undefined variables. Use `is defined`.
- Decrypt error: see below.
- Interrupted: rustow stopped between items. Run again.

## Exit code 2: configuration

Read the message. Common causes: missing `version = 1`, an unknown key, a rule on a non-template, a registered source that does not exist or is ignored, a bad mode string.

## Untrusted cache

For a file under `.rustowroot/`, rustow reports "untrusted cache" when a directory on the cache path is not owned by root or by the destination owner, or others can write to it. Deploy the file as a copy: use a template rule or `rustow file add`.

## Permission errors for system files

rustow never runs `sudo`. Run rustow with the needed rights. An unreadable destination gives a per-item error with a hint.

## age

- "no configured age identity can decrypt it": add the right identity file to `identity_files`.
- "not valid age ciphertext": the file is damaged or is not age.
- Identity file refused: run `chmod 600` on it. It must be yours, outside the repository and the cache.
- Passphrase-protected identity: use `--interactive`.

## GnuPG

- `gpg` not found: install GnuPG 2.2 or newer and put it in `PATH`.
- Recipient refused: use full fingerprints.
- "not trusted": set the key trust in your keyring. rustow does not change it.
- Locked key: unlock `gpg-agent` or use `--interactive`.
- Wrong home: set `[encryption.gpg] homedir` or `$GNUPGHOME`. You must own it. Others must not write to it.

## State file refused

rustow refuses corrupt, foreign or newer state. Move the file `<state_dir>/<repo-id>.json` away. Then run `rustow status`. Existing correct destinations appear as UNMANAGED. `rustow apply` adopts them.

## Stale edit workspace

After an `enc edit` conflict (exit 3), rustow keeps your plaintext in `<cache>/edit/edit-XXXX/`. Copy what you need. Then run `rustow cache clean`.
