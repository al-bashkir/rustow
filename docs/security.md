# Security

## What rustow does

- It never runs `sudo` and never runs commands from templates.
- It writes files with a temporary file in the same directory (mode 0600), then `fchown`/`fchmod`, `fsync` and `rename`. It never follows a symlink at the final destination.
- It follows a symlink in a destination ancestor only when it points to a directory that you or root own, and that others cannot write to. Sticky root directories are allowed.
- It never changes a destination that resolves inside the repository or the cache.
- It creates private cache (0700/0600) and state (0700/0600) directories. It refuses a cache root or state directory that you do not own.
- It checks identity files (owner, mode, location) and the GnuPG home.
- It never prints decrypted content, except in `diff` (explicit command) and in the editor workspace of `enc edit`.
- Error messages name templates and lines. They do not show values.

## Limits

- rustow does not erase data securely. After `rustow enc`, Git history, backups and deployed files keep the old plaintext.
- rustow has no transactional rollback. State is updated per successful item.
- The state file holds content hashes. Treat it as sensitive metadata.
- The cache holds decrypted and rendered content on disk, with mode 0600. Anyone who can read your account can read it.
- A cache-backed symlink in `.rustowroot/` is planned only when the cache path is trusted. Otherwise use a copy.
- Trust of GnuPG keys is never overridden.
- `diff` can print secrets.

## Reporting

Report security issues to the maintainers in private.
