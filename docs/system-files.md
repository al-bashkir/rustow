# System files

Files under `.rustowroot/` mirror `/`. `.rustowroot/etc/example/example.conf.tmpl` deploys to `/etc/example/example.conf`.

The `.rustowroot` directory itself is never deployed.

## Root target

`--root-target DIR` (or `defaults.root_target`) replaces `/`. rustow prints a notice when it is not `/`. Tests use it.

## Privileges

rustow never runs `sudo`. To write to `/etc` you must start rustow with the needed rights yourself. The state file belongs to the user that runs rustow, so use the same user each time, or set `--repo` and a state directory on purpose. An unreadable destination is an error for that item, with a privilege hint. A dry-run still lists the other items.

`owner` and `group` in a template rule need privilege. Without it, rustow fails with a privilege hint and leaves no partial file.

## Copy or symlink

Use a copy for system files. Set a `[[template_rules]]` entry, or use `rustow file add`.

Every symlink is checked before it is planned, in every scope. Its target is the cache entry (templates, decrypted files) or the repository file (ordinary links). Every path on the target chain must be owned by root or by the owner of the destination directory. See the [trust rule](security.md#trust-rule) for write access. When the check fails, rustow reports a conflict. The message starts with "untrusted link target: repository source: …; copy it instead with `rustow file add`", or with "untrusted link target: cache entry: …". So when you run rustow as root with a repository in your home, use copies for system files (`rustow file add`, or template metadata).

The cache inside the repository belongs to the repository owner. When another user (normally root) runs rustow, it uses a private cache in its own state directory (`<state_dir>/cache`) and prints a notice. Root and user runs never share or block one cache.

## Ignoring

Patterns in `.rustowignore` match repository paths. `/.rustowroot/etc/x` ignores `/etc/x`. An unanchored `*.bak` also matches inside `.rustowroot/`.

## Folding

rustow never folds directories in `.rustowroot/`.

## Example

```toml
[[template_rules]]
source = ".rustowroot/etc/example/example.conf.tmpl"
mode = "0644"
# owner = "root"
# group = "root"
```

```sh
rustow --root-target /tmp/root apply --dry-run
rustow apply .rustowroot
```
