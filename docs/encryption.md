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
- A passphrase-protected identity file asks on the terminal only with `--interactive`. `enc edit` implies `--interactive`.
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
- Each fingerprint must match one valid key with encryption capability in the local keyring. gpg picks the encryption subkey.
- rustow does not change trust. An untrusted key gives "not trusted".
- rustow checks the gpg status records. Symmetric-only, unprotected, truncated or mismatched ciphertext is refused.
- With a locked key and no `--interactive`, rustow fails fast. Unlock `gpg-agent` or use `--interactive`.

## Encrypt a file

```sh
rustow enc .ssh/secret.conf               # default backend
rustow enc --backend gpg .ssh/secret.conf
```

`enc` writes `PATH.age` or `PATH.gpg`. It does not overwrite an existing file (exit 3). It verifies by decrypting when a private key is available. Otherwise it prints "not verified". It moves `[[files]]` and `[[template_rules]]` entries to the new name. It removes the plaintext only if the plaintext did not change meanwhile.

`enc` prints a notice: Git history, backups and deployed files keep the old plaintext. rustow does not erase them.

`enc` rejects reserved paths, symlinks, missing files and already encrypted files (exit 2). It rejects empty or invalid recipients (exit 2). It has no `--dry-run`. A file named `edit` is written `./edit`.

## Edit an encrypted file

```sh
rustow enc edit [--reencrypt] .ssh/secret.conf.age
```

- The backend comes from the suffix. `enc edit` never converts.
- rustow decrypts into `<cache>/edit/edit-XXXX/` (mode 0700) as a 0600 file with the logical name.
- The editor is `$VISUAL`, else `$EDITOR`, else `vi`. rustow splits the command on whitespace. It does not use a shell.
- If the editor fails, the ciphertext stays unchanged and rustow removes the workspace.
- If the ciphertext changed meanwhile, rustow exits with 3. It leaves the ciphertext untouched and keeps your edited plaintext in the workspace. It prints the path. `rustow cache clean` removes stale workspaces.
- If content and recipients are unchanged, rustow does not rewrite the file.
- rustow detects recipient changes from the last recorded encryption, or from the recipient count in the ciphertext. `--reencrypt` forces a rewrite.
- It never deploys.

## Deployment

- Default: decrypt into the cache and link to it (`decrypt+link`).
- `rustow file add x.age`: decrypt and copy (`decrypt+copy`, mode 0600).
- Encrypted templates: decrypt, then render.
- Changing between plain and encrypted retargets rustow's own link automatically.
- `--dry-run` decrypts in memory only. Missing keys make the plan incomplete (exit 4).
- Ignored sources are never decrypted.
