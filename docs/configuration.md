# Configuration

The file is `<repo>/.rustow.toml`. It is optional. Without it, rustow uses the defaults. If the file exists, it must contain `version = 1`.

rustow rejects unknown keys everywhere, except under `[vars]`. A configuration error gives exit code 2.

## Full example

```toml
version = 1

[defaults]
target = "~"
root_target = "/"
cache_dir = ".rustowcache"
# state_dir = "~/.local/state/rustow"
create_directories = true
fold_directories = false

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
```

## `[defaults]`

| Key | Default | Meaning |
| --- | --- | --- |
| `target` | `"~"` | Target of home sources. `--target` overrides it. |
| `root_target` | `"/"` | Target of `.rustowroot/` sources. `--root-target` overrides it. |
| `cache_dir` | `".rustowcache"` | Cache directory. A relative path starts at the repository. |
| `state_dir` | none | State directory. Default: `$XDG_STATE_HOME/rustow`, else `~/.local/state/rustow`. |
| `create_directories` | `true` | Create missing parent directories. |
| `fold_directories` | `false` | Fold plain directories into one symlink. See below. |

### Folding

When `fold_directories = true`, rustow uses one symlink for the shallowest home directory when both conditions are true:

- The repository directory holds only ordinary link sources.
- The destination directory does not exist.

rustow never folds in `.rustowroot/`.

A recorded folded link that no longer qualifies becomes a real directory. This happens when you add a template, a copy, an encrypted file or an ignored file, or when you turn folding off. rustow refuses to unfold when files under the directory are not sources, because they would disappear from the destination.

`unlink` of a file inside a folded directory matches nothing. Unlink the directory.

## `[templates]`

| Key | Default | Meaning |
| --- | --- | --- |
| `extension` | `".tmpl"` | Template suffix. It must start with `.`. It must not end in `.age` or `.gpg`. |
| `engine` | `"minijinja"` | Only `"minijinja"` is valid. |

## `[encryption]`

See [encryption](encryption.md).

| Key | Meaning |
| --- | --- |
| `backend` | Default backend for `rustow enc`: `"age"` (default) or `"gpg"`. |
| `recipients` | age public keys (`age1...`). |
| `identity_files` | age identity files. |
| `[encryption.gpg] recipients` | Full GnuPG key fingerprints. |
| `[encryption.gpg] homedir` | GnuPG home directory. |

## `[vars]`

Free-form values. Templates read them as `vars.<name>`. Tables and lists are allowed.

## `[[files]]`

```toml
[[files]]
source = ".config/example/settings.json"
```

Registers a source for copy deployment. Use `rustow file add` to edit this list. See [file management](file-management.md).

## `[[template_rules]]`

```toml
[[template_rules]]
source = ".rustowroot/etc/example/example.conf.tmpl"
mode = "0644"
owner = "root"
group = "root"
```

A rule sets the metadata of a template and makes it deploy as a copy. Set at least one of `mode`, `owner`, `group`.

- `mode` is a string of 3 or 4 octal digits, for example `"0600"` or `"644"`. The maximum is `0777`. rustow rejects setuid, setgid and sticky bits.
- `owner` and `group` are a name or a numeric id. An unknown name is an error.

A rule on a file that is not a template is an error. A registered source or a rule source must exist. It must not be ignored or reserved.

## Deployment modes

| Mode | Label | Source |
| --- | --- | --- |
| Link | link | Ordinary file |
| FileCopy | copy | Listed in `[[files]]` |
| DecryptLink | decrypt+link | `.age` or `.gpg` file |
| DecryptCopy | decrypt+copy | Encrypted file listed in `[[files]]` |
| TemplateLink | template+link | Template without rule |
| TemplateCopy | template+copy | Template with a rule |

Encrypted templates (`name.tmpl.age`) also work. There is exactly one outer `.age` or `.gpg`. Nested names such as `x.age.gpg` are rejected. The destination name drops the encryption suffix, then the template extension.
