# System files

Files under `rustowroot/` mirror `/`. `rustowroot/etc/example/example.conf.tmpl` deploys to `/etc/example/example.conf`.

The `rustowroot` directory itself is never deployed.

## Root target

`--root-target DIR` (or `defaults.root_target`) replaces `/`. rustow prints a notice when it is not `/`. Tests use it.

## Privileges

Run rustow as yourself. Your HOME, your state, your keys and your cache stay in use. When a system file needs root, rustow asks `sudo` for it:

- rustow plans as you. A system file needs sudo when it must change and you cannot write its directory, or when it must belong to another user, or to a group that you are not a member of (for example a root-owned file in `/etc`, or `owner`/`group` in a template rule or a `[[files]]` entry). A file of yours may get any of your own groups (your primary group and your supplementary groups) without sudo.
- `apply`, `file apply` and `template apply` write the other items first. Then they run `sudo rustow __sudo-helper` once for all system files that need it. sudo asks for your password on the terminal. rustow renders and decrypts as you and sends the planned content to the helper on a pipe. A system file writes no cache entry. It reads a copied source again at that time. The helper checks the hash of the content against the plan before it replaces the file, so a source that changed during the run fails its item with "content changed while rustow was running". rustow sends one file at a time, and it waits for the result of that file before it sends the next one. So each result counts on its own: when sudo or the helper stops in the middle, the files that the helper reported stay recorded, and each later item fails. The helper writes each destination with the same code as a normal run. It opens the planned directory again with your trust rule, checks the destination again, writes through that open directory, and verifies the result. When the directory is not the planned one any more (for example, someone replaced it while sudo asked for the password), the item fails with "changed while rustow was running" and nothing is written. Your state records the result.
- `undeploy` removes such a system file, and the empty directories that rustow created for it, through the helper in the same way.
- The plan and the dry run show these items with "via sudo". A dry run, `status` and `diff` never run sudo.
- When sudo fails (wrong password, no terminal), each of these items fails with a message, for example "sudo failed (exit status: 1); system files that you cannot write need it". The other items stay done. The exit code is 4.
- A home file never uses sudo. A home destination that you cannot write fails with "…; elevated privileges may be required".
- `RUSTOW_SUDO` names another program, for example `doas`. rustow runs `$RUSTOW_SUDO -- <rustow> __sudo-helper`. A name without a `/` (also the default `sudo`) is looked up in the absolute directories of `PATH` only.
- For `--backup`, rustow makes the backup directories as you before sudo starts. The helper never makes a directory. It only moves the old file into that directory. So the backup file of a root-owned system file is owned by root, but its directory is yours: you can remove it, and later backups work.
- rustow chooses the name of each temporary file of the helper and journals it before the helper starts. When the helper dies in the middle of a file, the next run prints the command that removes the temporary file, for example "remove it with `sudo rm -- /etc/.rustow-tmp-…`". See [temporary files](cli.md#temporary-files-of-an-interrupted-run).
- The helper checks each job before it acts. It refuses a path that is not absolute or that has an empty, `.` or `..` part, a directory or temporary name that is not one name, and a mode with a setuid, setgid or sticky bit (as the configuration does). It writes nothing for such a job, and the item fails with "the sudo helper refuses this job: …". So an update through sudo of an existing file with such a bit fails too.
- Anyone who may run `sudo rustow` may write any file as root: do not allow rustow in sudoers without a password unless you mean that.

### Destinations that you cannot read

A system file can be unreadable for you, for example a `[[files]]` entry with `mode = "0640"`, `owner = "root"` and `group = "root"`. rustow plans as you, so it sees only the owner, the group and the mode of such a file. It compares the file with your state:

- The state records the content that rustow wrote last. When the desired content is that content and the metadata is as desired, the item is clean. `status` shows it as ok. With `-v`, the detail is "not readable by you; compared with the state". `apply` does not run sudo for it.
- When the desired content changed, the item is an update through sudo. The helper reads the file as root before it changes it. When the content is not the recorded one (someone changed it), the item is a MODIFIED conflict: "destination was modified since rustow last applied it", and the file stays. With `--force`, the helper replaces it. With `--backup`, it saves it first.
- When the state has no record of the file, the item is a conflict: "cannot read the destination; rustow has no record of it". `--force` and `--backup` replace it through sudo.
- `undeploy` removes such a file through the helper. The helper removes it only when its content is the recorded one. One item that fails never stops the other removals.
- `diff` cannot show the content. For such an item that would change, it prints "rustow: <dest>: cannot read the destination (Permission denied); elevated privileges may be required" on stderr and goes on with the other items. The exit code is 4.

Do not run rustow itself with `sudo`. Then HOME and the state directory are root's, so rustow would link your home files into `/root`. When rustow runs as root and `$SUDO_USER` names another user, every command except `completions` stops with exit 2: "do not run rustow with sudo: it would use root's home and root's state for the dotfiles of alice; run rustow as alice: it asks for sudo when a system file needs it (pass --target to deploy into root's home on purpose)". With an explicit `--target`, rustow runs. A real root login has no `$SUDO_USER`, so it is not affected.

## Copies only

A system file never deploys as a symlink. A symlink into a private cache is unusable: cache directories have mode 0700 and cache files 0600, so other programs get "Permission denied". A symlink into a user-writable repository or cache is unsafe: that user can change a system file.

- A template without a `[[template_rules]]` entry deploys as `template+copy`. A new file gets mode 0644, or 0600 when the template is encrypted. An update keeps the mode, owner and group of the existing file.
- rustow creates a missing system directory with mode 0755, whatever the umask. This is also true for an encrypted file: the file's own mode and group protect it, and the programs that read it must reach it.
- A template with a rule deploys as `template+copy` with the metadata of the rule. Settings in the template's [meta block](templates.md#meta-block), such as `{%- meta.owner = "root" %}`, do the same without a rule.
- An ordinary or encrypted file must be registered with `rustow file add` or in `[[files]]`. It then deploys as `copy` or `decrypt+copy`. A `[[files]]` entry can set `mode`, `owner` and `group`, as a template rule does. `rustow file add` writes the state of the user who runs it. Run it as the user who runs `apply`, or use `[[files]]`. See [file management](file-management.md#state-or-rustowtoml).
- An ordinary or encrypted file that is not registered does not deploy. rustow skips it and deploys the other sources. The exit code does not change. The warning tells you what to do: "warning: rustowroot/etc/x: system files deploy only as copies; register it with `rustow file add rustowroot/etc/x`". `status` shows the file as SKIPPED. You can leave a file unregistered on purpose.
- A command that names such a file, for example `rustow apply rustowroot/etc/x`, stops with exit 2: "source 'rustowroot/etc/x': system files deploy only as copies; register it with `rustow file add rustowroot/etc/x`". A directory argument, for example `rustowroot/etc/`, only warns.
- `rustow file add` works also while other system files are not registered yet.
- An ignored file gets no warning: rustow skips it before this check.

`rustow link` and `rustow unlink` take symlinks only, so they never deploy or remove a system file. `rustow file apply`, `rustow template apply` and `rustow apply` deploy system files. `rustow undeploy` removes them.

The [trust rule](security.md#trust-rule) checks the repository source of each system file that deploys. rustow installs it as root, so a source that another user can change is a conflict: "untrusted source: …; another user can change what rustow installs as root". `--force` and `--backup` do not resolve it. `check` reports it.

## Cache of other users

The cache inside the repository belongs to the repository owner. When another user (normally root) runs rustow, it uses a private cache in its own state directory (`<state_dir>/cache`) and prints a notice. Root and user runs never share or block one cache.

## Ignoring

Patterns in `.rustowignore` match repository paths. `/rustowroot/etc/x` ignores `/etc/x`. An unanchored `*.bak` also matches inside `rustowroot/`.

## Folding

rustow never folds directories in `rustowroot/`.

## Example

```toml
[[template_rules]]
source = "rustowroot/etc/example/example.conf.tmpl"
mode = "0644"
# owner = "root"
# group = "root"

[[files]]
source = "rustowroot/etc/example/app.conf"
mode = "0640"
# owner = "root"
# group = "root"
```

```sh
rustow --root-target /tmp/root apply --dry-run
rustow apply rustowroot
```
