# Templates

A template is a source file whose name ends in the template extension (default `.tmpl`). rustow renders it with MiniJinja and deploys the result. The destination name drops the extension. `.gitconfig.tmpl` becomes `~/.gitconfig`.

## Deployment

- Without a rule, a home template deploys as `template+link`. rustow writes the rendered file to the cache and links to it.
- With a `[[template_rules]]` entry, a template deploys as `template+copy`. The rule sets `mode`, `owner` and `group`.
- A system template under `.rustowroot/` always deploys as `template+copy`, also without a rule. See [system files](system-files.md).

```toml
[[template_rules]]
source = ".ssh/config.tmpl"
mode = "0600"
```

A new copy gets the rule `mode`. Without a rule `mode` (or without a rule, for a system template), a new copy gets 0644 (0600 if the source is encrypted), and an update keeps the mode of the existing file. The rule `mode` takes octal digits only and must include owner read (0400). Otherwise it is a configuration error (exit 2).

If you edited the cache entry through the link and then add a rule, `apply` reports a conflict on the cache entry. `--backup` saves your edit under `<state_dir>/backups/<repo-id>/<time>/cache/<dest>`.

## Syntax

```jinja
[user]
    name = {{ vars.git.user_name }}
{%- if os == "macos" %}
    fsmonitor = true
{%- endif %}
```

## Variables

| Name | Value |
| --- | --- |
| `os` | `"linux"` or `"macos"` |
| `arch` | CPU architecture |
| `family` | `"unix"` |
| `hostname` | Host name |
| `username` | User name |
| `uid`, `gid` | Numeric ids |
| `home_dir` | The home target |
| `vars` | Values from `[vars]` |

## Rules of the engine

- An undefined variable is an error. This is true also in `if`. Use `{% if vars.x is defined %}`.
- rustow does not escape output automatically.
- A trailing newline stays.
- A template cannot include or import files.
- A template cannot run commands.
- Errors name the template and the line. They never show values.

## Commands

```sh
rustow template list          # list templates and their mode
rustow template apply         # deploy templates only
rustow check                  # renders plaintext templates
rustow diff                   # shows what would change
```

Encrypted templates (`name.tmpl.age`) are decrypted first, then rendered. `check` never decrypts them.

## Cache

Rendered files go to `<cache>/rendered/<repo-id>/<scope-id>/<dest>`. The directories have mode 0700 and the files 0600. If you edit the deployed file through the symlink, you edit the cache entry. `status` shows the item as MODIFIED. `apply` reports a conflict: "cache entry: destination was modified since rustow last applied it". `--dry-run` writes no cache.

### When rustow renders again

rustow renders each selected template again on every run that plans it: `apply`, `template apply`, `status` and `diff`. The output changes when one of these changes:

- The template file. For an encrypted template, this is the decrypted text.
- A value in `[vars]`.
- A built-in variable, for example `hostname`, `uid` or `home_dir`.

rustow compares the hash of the new output with the cache entry. It writes the cache entry only when the content is different. Unchanged output is not rewritten. A `template+copy` destination follows the same rule.

The cache path includes the repository id and the scope id. A moved repository, or another `--target` or `--root-target`, uses a new cache entry.
