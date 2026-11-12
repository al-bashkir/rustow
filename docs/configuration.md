# Configuration

The file is `<repo>/.rustow.toml`. It is optional. Without it, rustow uses the defaults. If the file exists, it must contain `version = 1`.

rustow rejects unknown keys everywhere, except under `[vars]` and the `vars` of a `[[global_rules]]` entry. A configuration error gives exit code 2. A problem with one source or one `[[files]]` or `[[template_rules]]` entry is a warning instead: rustow skips that source and deploys the rest. See [skipped sources](cli.md#skipped-sources).

A TOML error reads `.rustow.toml: line L, column C, key <path>: <message>`, for example `.rustow.toml: line 7, column 22, key defaults.create_directories: invalid type, expected a boolean`. The key part is missing when rustow cannot find the key. The message never shows the source line or a value. Other validation messages can name values that are not secret, such as a `mode`, an `owner`, the `engine`, the `extension` or a source path. rustow never shows `[vars]` values.

## Full example

```toml
version = 1

[defaults]
target = "~"
root_target = "/"
cache_dir = ".rustowcache"
# state_dir = "~/.local/state/rustow"
create_directories = true
fold_directories = true

[templates]
extension = ".tmpl"
engine = "minijinja"

[encryption]
backend = "age"
recipients = []
identity_files = ["~/.config/rustow/age/identity.txt"]

[encryption.gpg]
recipients = []
# homedir = "~/.gnupg"

[vars]
editor = "nvim"

[vars.git]
user_name = "Example User"

[[files]]
source = ".config/example/settings.json"

[[template_rules]]
source = ".ssh/config.tmpl"
mode = "0600"

[[global_rules]]
os = "macos"
vars = { editor = "vim" }
ignore = [".config/foot/"]
```

## `[defaults]`

| Key                  | Default          | Meaning                                                                                                                                                                                                       |
| -------------------- | ---------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `target`             | `"~"`            | Target of home sources. `--target` overrides it.                                                                                                                                                              |
| `root_target`        | `"/"`            | Target of `.rustowroot/` sources. `--root-target` overrides it.                                                                                                                                               |
| `cache_dir`          | `".rustowcache"` | Cache directory. A relative path starts at the repository. An in-repository cache belongs to the repository owner: another user (normally root) uses `<state_dir>/cache` instead, and rustow prints a notice. |
| `state_dir`          | none             | State directory. Default: `$XDG_STATE_HOME/rustow`, else `~/.local/state/rustow`.                                                                                                                             |
| `create_directories` | `true`           | Create missing parent directories.                                                                                                                                                                            |
| `fold_directories`   | `true`           | Fold plain directories into one symlink. Never a directory directly under the home target or under `~/.local`. See below.                                                                                     |

### Cache and state directories

rustow compares `cache_dir` and `state_dir` with the repository in canonical form. So it also finds a directory inside the repository when the path goes through a symlink, for example a symlinked repository path, or a HOME behind a symlink such as `/home -> /var/home`.

- rustow never deploys a cache or state directory inside the repository.
- A cache or state directory must not be the repository or one of its parents. Inside the repository, it must not be in `.rustowroot` or `.git`. Otherwise every command stops with exit 2.
- A configured in-repository `cache_dir` or `state_dir` may hold only rustow's names: `rendered/`, `decrypted/`, `edit/`, `backups/`, `cache/`, `<repo-id>.json`, `<repo-id>.lock` and temporary files that start with `.rustow-tmp-`. Any other name stops every command with exit 2. Thus `cache_dir = ".config"` cannot hide your `.config` sources.
- The built-in `.rustowcache` is exempt from that content rule. It is reserved by name, so a file such as `.DS_Store` in it does no harm.
- A symlink or a non-directory above an in-repository cache or state directory stops every command with exit 3.

### Folding

rustow uses one symlink for the shallowest home directory when all these conditions are true:

- The directory is not directly under the home target, and not directly under `~/.local`. `~/.config/nvim` and `~/.local/share/fonts` can fold. `~/.config`, `~/.ssh`, `~/.gnupg`, `~/.local`, `~/.local/share`, `~/.local/state` and `~/.local/bin` never fold. Other directories below them can (see the warning below).
- The repository directory holds only ordinary link sources, and each of them may fold: its [`[[file_rules]]`](#file_rules) value, or `fold_directories` when no rule covers it, is `true`.
- It holds no reserved directory, such as `.git` or an in-repository `cache_dir` or `state_dir`. This is also true for a configured directory that does not exist yet.
- No destination collision is on the directory or below it.
- The destination directory does not exist.
- For a new fold, the run selects every source below the directory. A selective run (a path argument, or a command such as `rustow link`) folds a new directory only then. It keeps an existing fold.

rustow never folds in `.rustowroot/`.

Why these never fold: on a new machine, `~/.ssh`, `~/.gnupg`, `~/.config` or `~/.local/share` often does not exist yet. A fold there would make ssh, GnuPG, keyrings, shell history and every program that writes to `~/.config` write private files into the repository. The same risk applies to any folded directory: a program that writes a new file into it writes into the repository. The rule protects only the directories named above. Other directories can still fold on a new machine, for example `~/.config/gh` (`gh auth login` writes its token to `hosts.yml`) or `~/.config/Code`. Use `[[file_rules]]` with `fold_directories = false` for each directory that programs write to.

A recorded folded link that no longer qualifies becomes a real directory. This happens when you add a template, a copy, an encrypted file or an ignored file, or when you turn folding off. rustow refuses to unfold when files under the directory are not sources, because they would disappear from the destination.

rustow unfolds only when the same run deploys every source below the directory again. A partial run cannot unfold: a command that deploys only some modes, a path argument, or an item below the directory that is in conflict or fails. Then the folded link is a conflict, "cannot unfold the folded directory: X would not be deployed again; run `rustow apply <dir>`". Each item below it is a conflict too: "the folded directory D cannot be unfolded yet". The command exits with 3. Run `rustow apply <dir>` to unfold.

If you replace rustow's folded link yourself (with a directory or your own symlink), rustow treats it as yours: it plans the files below it against what is there, with the normal conflict rules, and `unlink` forgets the old record.

`unlink` of a file inside a folded directory matches nothing. Unlink the directory.

## `[templates]`

| Key         | Default       | Meaning                                                                       |
| ----------- | ------------- | ----------------------------------------------------------------------------- |
| `extension` | `".tmpl"`     | Template suffix. It must start with `.`. It must not end in `.age` or `.gpg`. |
| `engine`    | `"minijinja"` | Only `"minijinja"` is valid.                                                  |

## `[encryption]`

See [encryption](encryption.md).

| Key                           | Meaning                                                         |
| ----------------------------- | --------------------------------------------------------------- |
| `backend`                     | Default backend for `rustow enc`: `"age"` (default) or `"gpg"`. |
| `recipients`                  | age public keys (`age1...`).                                    |
| `identity_files`              | age identity files.                                             |
| `[encryption.gpg] recipients` | Full GnuPG key fingerprints.                                    |
| `[encryption.gpg] homedir`    | GnuPG home directory.                                           |

## `[vars]`

Free-form values. Templates read them as `vars.<name>`. Tables and lists are allowed. A matching [`[[global_rules]]`](#global_rules) entry can change them for one host.

## `[[files]]`

```toml
[[files]]
source = ".config/example/settings.json"
```

Registers a source for copy deployment. Use `rustow file add` to edit this list. See [file management](file-management.md). A system file under `.rustowroot/` that is not a template must be in this list. See [system files](system-files.md).

## `[[template_rules]]`

```toml
[[template_rules]]
source = ".rustowroot/etc/example/example.conf.tmpl"
mode = "0644"
owner = "root"
group = "root"
```

A rule sets the metadata of a template and makes it deploy as a copy. Set at least one of `mode`, `owner`, `group`. A system template under `.rustowroot/` deploys as a copy also without a rule.

- `mode` is a string of 3 or 4 octal digits, for example `"0600"` or `"644"`. Only the digits 0–7 are allowed, with no sign or space. The maximum is `0777`. rustow rejects setuid, setgid and sticky bits. The mode must include owner read (`0400`), because rustow reads the deployed file to find changes. Any other mode is a configuration error (exit 2).
- `owner` and `group` are a name or a numeric id. An unknown name is an error. `rustow check` reports it as a problem.

A rule on a file that is not a template skips that file with a warning. A registered source or a rule source must exist. It must not be ignored, reserved or a directory. Otherwise rustow skips the entry with a warning, for example "warning: gone.json: .rustow.toml [[files]] entry does not exist (remove the entry, e.g. with `rustow file remove`)". A template in `[[files]]` is skipped too.

## `[[file_rules]]`

```toml
[[file_rules]]
source = ".config/nvim"          # a directory: every source below it
fold_directories = false

[[file_rules]]
source = ".config/a/b.txt"       # one file
fold_directories = false
```

A rule controls [folding](#folding) for symlinked files. `source` is a file or a directory. `fold_directories` is required. Two rules with the same `source` are a configuration error (exit 2).

Each home link source gets the value of the rule on its own path or on its nearest parent directory. With no rule, it gets `defaults.fold_directories`. A directory can fold only when every link source below it has the value `true`.

| Repository                                        | Rules                                             | Result                                                                        |
| ------------------------------------------------- | ------------------------------------------------- | ----------------------------------------------------------------------------- |
| `.config/a/b.txt`, `.config/a/c/b.txt`            | `.config/a/b.txt` = false                         | `.config/a` does not fold. `b.txt` gets its own symlink. `.config/a/c` folds. |
| `.config/nvim/init.lua`, `.config/nvim/lua/x.lua` | `.config/nvim` = false                            | Nothing at or below `.config/nvim` folds. Each file gets its own symlink.     |
| same                                              | `.config/nvim` = false, `.config/nvim/lua` = true | `.config/nvim/lua` folds. `init.lua` gets its own symlink.                    |

With `fold_directories = false`, a rule with `true` lets a directory fold. A directory directly under the home target or under `~/.local` never folds, whatever the rules say.

A rule that stops an existing fold unfolds it, with the rules above. A directory that an unfold has made real keeps per-file links; it does not fold again.

An entry that cannot apply gives a warning, and rustow ignores the entry. This happens when the source does not exist, is ignored or reserved, is under `.rustowroot/`, or is a file that is not a plain symlinked file (a template, a copy or an encrypted file). Example: "warning: gone: .rustow.toml [[file_rules]] entry does not exist (remove the entry)". The warning shows on stderr, in the JSON `warnings` array and in `rustow check`. `status` shows no row for it. A directory rule that covers templates or copies is valid; the rule has no effect on them.

## `[[global_rules]]`

```toml
[vars]
user = "alice"
uid = 1000

[vars.git]
name = "Alice"
email = "alice@home.example"

[[global_rules]]
os = "macos"
vars = { uid = 501 }
ignore = [".config/foot/"]

[[global_rules]]
hostname = "work"
vars = { user = "asmith", git = { email = "asmith@work.example" } }
ignore = ["/.gitconfig"]
```

A rule changes `[vars]` and the ignore rules on some hosts only.

- **Conditions:** `os`, `arch`, `family`, `hostname` and `username`. They have the same values as the [template variables](templates.md#variables). Each value is an exact string. A rule matches when all its conditions are true. A rule must have at least one condition.
- **`vars`:** a table. A matching rule merges it over `[vars]`. Tables merge key by key. Any other value replaces the old value. Keys that the rule does not set keep their value. When two matching rules set the same key, the lower rule in the file wins. In the example, on the host `work`, `vars.git.name` stays `"Alice"` and `vars.git.email` becomes `"asmith@work.example"`.
- **`ignore`:** a list of patterns in [ignore](ignore.md) syntax. A matching rule adds them to `.rustowignore`. Each rule works alone: a `!` pattern in a rule cannot re-include a path that `.rustowignore` or another rule ignores. rustow checks the patterns of every rule, also of rules that do not match this host.

A path that a matching rule ignores is not for this host:

- rustow does not deploy it and prints no warning. A `[[files]]`, `[[template_rules]]` or `[[file_rules]]` entry for it gives no warning either.
- `status --ignored` shows it as IGNORED.
- A path argument that names it exits 2: "'.gitconfig' is ignored by [[global_rules]] #2 (hostname == "work")".
- If it was deployed before, its destination is ORPHANED, for example "orphaned ~/.gitconfig <- .gitconfig (link): ignored by [[global_rules]] #2 (hostname == "work"); `rustow undeploy .gitconfig` removes it". rustow removes nothing.

A rule without conditions, an unknown key or a bad pattern is a configuration error (exit 2). The message names the rule by its number in the file, from 1. It never shows a `vars` value.

## Deployment modes

| Mode         | Label         | Source                                   |
| ------------ | ------------- | ---------------------------------------- |
| Link         | link          | Ordinary home file                       |
| FileCopy     | copy          | Listed in `[[files]]`                    |
| DecryptLink  | decrypt+link  | `.age` or `.gpg` home file               |
| DecryptCopy  | decrypt+copy  | Encrypted file listed in `[[files]]`     |
| TemplateLink | template+link | Home template without rule               |
| TemplateCopy | template+copy | Template with a rule, or system template |

A system file under `.rustowroot/` never deploys as a link. An ordinary or encrypted system file that is not in `[[files]]` does not deploy: rustow skips it with a warning. See [system files](system-files.md).

Encrypted templates (`name.tmpl.age`) also work. There is exactly one outer `.age` or `.gpg`. rustow skips nested names such as `x.age.gpg` with a warning. The destination name drops the encryption suffix, then the template extension.

The state file and mode selection use these labels. Output names the decrypt step and the backend of an encrypted source: `decrypt(age)+link`, `decrypt(gpg)+copy`, `decrypt(gpg)+template+link`. Plans (text and JSON), `status`, `undeploy`, `template list` and `file list` show these labels.

Two sources that map to one destination collide, for example `x`, `x.age` and `x.tmpl`. A destination below another destination collides too. On a case-insensitive target filesystem, names that differ only in case collide. rustow tests each target for this. A root target of `/` always counts as case-sensitive (known limit).
