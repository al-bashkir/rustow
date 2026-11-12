# Encryption

rustow supports two backends. A source with the outer suffix `.age` uses age. A source with the outer suffix `.gpg` uses GnuPG. rustow never falls back to the other backend or to plaintext.

## age

rustow includes age. It uses X25519 recipients.

```sh
age-keygen -o ~/.config/rustow/age/identity.txt
chmod 600 ~/.config/rustow/age/identity.txt
```

```toml
[encryption]
backend = "age"
recipients = ["age1..."]
identity_files = ["~/.config/rustow/age/identity.txt"]
```

- An identity file path is absolute or starts with `~/`.
- The identity file must be a regular file that you own. Group and others must have no access. Fix with `chmod 600`.
- The identity file must be outside the repository and the cache.
- A passphrase-protected identity file asks for its passphrase on the terminal only with the global `--interactive` flag. See [Interactive and unattended runs](#interactive-and-unattended-runs).
- rustow rejects scrypt-only age files.
- A failure message tells "no configured age identity can decrypt it" apart from "not valid age ciphertext".

## GnuPG

rustow runs the installed `gpg` (GnuPG 2.2 or newer). It finds `gpg` in `PATH`. You cannot configure the path. On macOS, run `brew install gnupg`. rustow needs `gpg` only when it processes a selected, non-ignored `.gpg` source.

```toml
[encryption]
backend = "gpg"

[encryption.gpg]
recipients = ["0123456789ABCDEF0123456789ABCDEF01234567"]
homedir = "~/.gnupg"
```

- Recipients are full fingerprints (40 or 64 hex digits). rustow refuses names, emails and short ids.
- `homedir` is the config value, else `$GNUPGHOME`, else `~/.gnupg`. You must own it. Others must not write to it. It must be outside the repository and the cache.
- Each fingerprint must match one valid primary key with encryption capability in the local keyring. gpg picks the encryption subkey. A subkey fingerprint is refused: "<fpr> is a subkey fingerprint; configure the primary key fingerprint <primary>".
- rustow never fetches keys. It runs `gpg` with `--no-options`, so `gpg.conf` has no effect.
- rustow does not change trust. The key validity must be marginal, full or ultimate. rustow checks this before it encrypts or opens the editor. An untrusted key gives "not trusted" (exit 4).
- rustow checks the gpg status records. Symmetric-only, unprotected, truncated or mismatched ciphertext is refused.
- A locked key fails fast unless you use `--interactive`. See [Interactive and unattended runs](#interactive-and-unattended-runs).

## Interactive and unattended runs

By default rustow never asks for a passphrase, so it can run from scripts and timers.

- GnuPG: rustow runs `gpg --batch --no-tty --pinentry-mode error`. A key that `gpg-agent` has already unlocked works. A locked key fails at once, with the hint "if the key is passphrase-protected, unlock it in gpg-agent or rerun with --interactive".
- age: a passphrase-protected identity file fails with "could not unlock a passphrase-protected identity file (wrong passphrase, or rerun with --interactive)".
- The failed item is an error. `apply`, `diff` and `enc edit` exit with 4. `status` shows ERROR.
- With the global `--interactive` flag, rustow runs gpg with `--pinentry-mode ask`, so `gpg-agent` starts its pinentry. For age, rustow asks for the identity passphrase on the terminal.
- The rule is the same for every command. `enc edit` does not imply `--interactive`. Without it, a locked key fails with exit 4 before the editor starts. The editor itself always runs on your terminal.

```sh
rustow --interactive apply
rustow --interactive enc edit .ssh/secret.conf.gpg
```

## Mixed age and GnuPG repositories

One repository can hold `.age` and `.gpg` sources together.

- The outer suffix selects the backend for deployment, `diff`, `status` and `enc edit`. rustow never tries the other backend.
- `encryption.backend` and `rustow enc --backend` choose only the backend for new encryption.
- Each backend has its own configuration: `recipients` and `identity_files` for age, `[encryption.gpg]` for GnuPG. Configure each backend that you use.
- rustow loads age identities and runs `gpg` only for selected, non-ignored sources of that backend. On a machine without `gpg`, ignore the `.gpg` sources, or give paths to other sources. Otherwise the `.gpg` items fail, and under the default policy rustow writes nothing.
- `x.age` and `x.gpg` deploy to the same destination `x`. This is a collision: `check` reports it, and both items are conflicts (exit 3). A plaintext `x` next to `x.age` collides too. Keep one source for each destination.

## Encrypt a file

```sh
rustow enc .ssh/secret.conf               # default backend
rustow enc --backend gpg .ssh/secret.conf
```

`enc` writes `PATH.age` or `PATH.gpg`. It does not overwrite an existing file (exit 3). It verifies by decrypting when a private key is available. Otherwise it prints "not verified". It moves `[[files]]` and `[[template_rules]]` entries to the new name. It keeps the comments around `source` and CRLF line endings in `.rustow.toml`. It removes the plaintext only if the plaintext did not change meanwhile.

`enc` prints a notice: Git history, backups and deployed files keep the old plaintext. rustow does not erase them.

`enc` rejects reserved paths, symlinks, missing files and already encrypted files (exit 2). It rejects empty or invalid recipients (exit 2). It has no `--dry-run`. A file named `edit` is written `./edit`.

`enc` and `enc edit` refuse a path through a symlinked parent directory (exit 2), for example: "source 'ld' is a symbolic link; rustow encrypts only files inside the repository; use the real path".

## Edit an encrypted file

```sh
rustow enc edit [--reencrypt] .ssh/secret.conf.age
```

- The backend comes from the suffix. `enc edit` never converts.
- It refuses nested suffixes such as `x.age.gpg` (exit 2, "nested encryption suffixes are not supported"). It also refuses a name that is empty after rustow removes the suffixes, such as `.age` or `.tmpl.age` (exit 2).
- It checks the recipients of the backend before it decrypts. Empty or invalid recipients give exit 2. A GPG fingerprint that is not in the keyring, or a key that is not trusted (validity unknown), revoked, expired or cannot encrypt, gives exit 4. Then the editor does not start and rustow leaves no workspace.
- rustow decrypts into `<cache>/edit/edit-XXXX/` (mode 0700) as a 0600 file with the logical name.
- The editor is `$VISUAL`, else `$EDITOR`, else `vi`. rustow splits the command on whitespace. It does not use a shell.
- If the editor fails, the ciphertext stays unchanged and rustow removes the workspace.
- If the encryption fails after the edit (for example, the gpg run fails), rustow exits with 4. The ciphertext stays unchanged and rustow keeps your edited plaintext in the workspace. It prints the path.
- If the ciphertext changed meanwhile, rustow exits with 3. It leaves the ciphertext untouched and keeps your edited plaintext in the workspace. It prints the path. `rustow cache clean` removes stale workspaces.
- If content and recipients are unchanged, rustow does not rewrite the file.
- rustow finds a recipient change from the last encryption that the state file records for this ciphertext. Without such a record, GnuPG compares the key ids in the ciphertext with the configured keys, so rustow encrypts again also when you replace one key with another. age shows only the number of recipients, so rustow finds only a changed count. After you replace an age recipient, use `--reencrypt`. `--reencrypt` always forces a rewrite.
- It never deploys.

## Deployment

- Default: decrypt into the cache and link to it (`decrypt+link`).
- `rustow file add x.age`: decrypt and copy (`decrypt+copy`, mode 0600).
- Encrypted templates: decrypt, then render.
- Output names the backend in the mode label: `decrypt(age)+link`, `decrypt(gpg)+copy`, `decrypt(gpg)+template+link`. Plans (text and JSON), `status`, `undeploy`, `template list` and `file list` show these labels.
- Changing between plain and encrypted retargets rustow's own link automatically.
- `--dry-run` decrypts in memory only. Missing keys make the plan incomplete (exit 4).
- Ignored sources are never decrypted.
