# Security

## What rustow does

- It runs `sudo` only to start its own helper, and only for system files that you cannot write (see [system files](system-files.md#privileges)). It never runs commands from templates.
- It finds `gpg`, the sudo program and the `enc edit` editor only in the absolute directories of `PATH`, and it starts the program by its absolute path. It skips an empty or relative `PATH` entry (`.`, `bin`), because such an entry names the current directory, often the repository. A name with a `/` (for example `VISUAL=./ed`) starts as given.
- It writes files with a temporary file in the same directory (mode 0600), then `fchown`/`fchmod`, `fsync` and `rename`. It never follows a symlink at the final destination.
- It deploys system files (`rustowroot/`) only as copies. A symlink into a private cache is unusable for other programs. A symlink into a user-writable repository or cache is unsafe.
- It checks every symlink before it plans it, and every symlinked ancestor of a destination before it follows it. See [Trust rule](#trust-rule).
- It never changes a destination that resolves inside the repository or the cache.
- It creates private cache (0700/0600) and state (0700/0600) directories. It refuses a cache root or state directory that you do not own. Below the cache root, each directory must be a real directory that you own: rustow refuses a symlink there (for example `.rustowcache/rendered -> /etc`), so cache content cannot be written outside the cache.
- It checks identity files (owner, mode, location) and the GnuPG home.
- It never prints decrypted content, except in `diff` (explicit command) and in the editor workspace of `enc edit`.

## Trust rule

A symlink must point only at a file that other users cannot change. rustow checks each path on the chain of the link target: the repository source of an ordinary link, or the cache entry of a template or a decrypted file. Only home files deploy as symlinks, so the rule applies to home files. System files are always copies.

- Each path must be owned by root or by the owner of the destination directory.
- No path may be writable by others. A sticky directory that root owns, such as `/tmp`, is the exception.
- A path may be writable by its group only when that group is the user-private group of the owner: the primary group of the owner, with the owner's name and no other members. Thus a repository made with the default umask 002 of such systems is trusted.

When a check fails, the item is a conflict. The message starts with "untrusted link target: repository source: …" or "untrusted link target: cache entry: …". It gives the path and the cause, for example "writable by others" or "writable by group N, which is not the private group of uid M; run `chmod g-w <path>`". It also tells you to copy the file instead.

A destination can have a symlinked ancestor directory, for example `~/.config` that points to another disk. rustow opens the directories above a destination one at a time and never follows a symlink by accident. It follows a symlinked ancestor only when both checks pass for the current user:

- The symlink itself is owned by root or by you. Its mode bits do not count. Otherwise the item is a conflict: "symlink is not safe: …".
- Its target directory passes the same rule. Otherwise the item is a conflict: "symlink target is not safe: …".

A walk follows at most 40 symlinks. A dangling symlink blocks the item: "symlink does not resolve".

## Error messages

- Configuration (TOML) errors show only the file, the line, the column and the key path. They never show the source line or a value.
- Validation messages can name schema values that are not secret, such as `mode`, `owner`, `engine`, `extension` and source paths.
- Error messages never show `[vars]` values.
- Template errors name the template and the line.

## Limits

- rustow does not erase data securely. After `rustow enc`, Git history, backups and deployed files keep the old plaintext.
- rustow has no transactional rollback. rustow saves the state file once, at the end of the run, also when SIGINT, SIGTERM or SIGHUP stops the run. After SIGKILL, rustow saves no record of the run. The next `apply` adopts the destinations that are correct.
- The state file holds content hashes. Treat it as sensitive metadata.
- The cache holds decrypted and rendered content on disk, with mode 0600, until `rustow cache clean` (see [plaintext lifetime](encryption.md#deployment)). Anyone who can read your account can read it.
- Trust of GnuPG keys is never overridden.
- `diff` can print secrets. `diff --json` includes the content too.

## Reporting

Report security issues to the maintainers in private.
