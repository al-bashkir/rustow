# System files

Files under `.rustowroot/` mirror `/`. `.rustowroot/etc/example/example.conf.tmpl` deploys to `/etc/example/example.conf`.

The `.rustowroot` directory itself is never deployed.

## Root target

`--root-target DIR` (or `defaults.root_target`) replaces `/`. rustow prints a notice when it is not `/`. Tests use it.

## Privileges

rustow never runs `sudo`. To write to `/etc` you must start rustow with the needed rights yourself. The state file belongs to the user that runs rustow, so use the same user each time, or set `--repo` and a state directory on purpose. An unreadable destination is an error for that item, with a privilege hint. A destination in a directory that you cannot write is an error too: "cannot write to directory X: Permission denied …; elevated privileges may be required". rustow finds it in the plan, so the dry run and `status` show it. A dry-run still lists the other items. `apply` exits with 4, and under the default policy it writes nothing.

`owner` and `group` in a template rule need privilege. Without it, rustow fails with a privilege hint and leaves no partial file.

## Copies only

A system file never deploys as a symlink. A symlink into a private cache is unusable: cache directories have mode 0700 and cache files 0600, so other programs get "Permission denied". A symlink into a user-writable repository or cache is unsafe: that user can change a system file.

- A template without a `[[template_rules]]` entry deploys as `template+copy`. A new file gets mode 0644, or 0600 when the template is encrypted. An update keeps the mode, owner and group of the existing file.
- A template with a rule deploys as `template+copy` with the metadata of the rule.
- An ordinary or encrypted file must be registered with `rustow file add`. It then deploys as `copy` or `decrypt+copy`.
- An ordinary or encrypted file that is not registered is a configuration error (exit 2). Every command that reads all sources stops and writes nothing, for example `apply`, `diff`, `status` and `check`. The message tells you what to do: "source '.rustowroot/etc/x': system files deploy only as copies; register it with `rustow file add .rustowroot/etc/x`".
- `rustow file add` works also while other system files are not registered yet.
- rustow skips an ignored file before this check.

`rustow link` and `rustow unlink` take symlinks only, so they never deploy or remove a system file. `rustow file apply`, `rustow template apply` and `rustow apply` deploy system files. `rustow undeploy` removes them.

The [trust rule](security.md#trust-rule) checks symlinks. System files are copies, so it does not apply to them.

## Old symlinks

An older rustow deployed some system files as symlinks. rustow replaces such a symlink with a copy:

- A template symlink becomes a copy on the next `apply`.
- An ordinary or encrypted symlink becomes a copy after `rustow file add`. Until then, the configuration error stops every command that reads all sources, and the symlink stays.
- If you edited the file through the old symlink, you edited the cache entry. Then `apply` reports a conflict: "cache entry: destination was modified since rustow last applied it". `--backup` saves your edit under `<state_dir>/backups/<repo-id>/<time>/cache/<dest>`. `--force` discards it.

## Cache of other users

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
