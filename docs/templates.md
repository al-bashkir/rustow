# Templates

A template is a source file whose name ends in the template extension (default `.tmpl`). rustow renders it with MiniJinja and deploys the result. The destination name drops the extension. `.gitconfig.tmpl` becomes `~/.gitconfig`.

## Deployment

- Without a rule, a home template deploys as `template+link`. rustow writes the rendered file to the cache and links to it.
- With a `[[template_rules]]` entry, a template deploys as `template+copy`. The rule sets `mode`, `owner` and `group`.
- With settings in its [meta block](#meta-block), a template deploys as `template+copy` with those settings.
- A system template under `.rustowroot/` always deploys as `template+copy`, also without a rule. See [system files](system-files.md).

```toml
[[template_rules]]
source = ".ssh/config.tmpl"
mode = "0600"
```

A new copy gets the rule `mode`. Without a rule `mode` (or without a rule, for a system template), a new copy gets 0644 (0600 if the source is encrypted), and an update keeps the mode of the existing file. The rule `mode` takes octal digits only and must include owner read (0400). Otherwise it is a configuration error (exit 2).

If you edited the cache entry through the link and then add a rule, `apply` reports a conflict on the cache entry. `--backup` saves your edit under `<state_dir>/backups/<repo-id>/<time>/cache/<dest>`.

## Meta block

A template can hold its own metadata and deploy conditions at the top of the file. Then `.rustow.toml` needs no `[[template_rules]]` entry for it.

```jinja
{%- meta.os == "linux" %}
{%- meta.uid = 1001 %}
{%- meta.gid = 1001 %}
{%- meta.mode = "0600" %}
{%- if hostname == "laptop" %}
some content
{%- elif hostname == "pc" %}
other content
{%- endif %}
```

The meta block is the group of `meta.` lines at the start of the file. It stops at the first line that is not a meta line. rustow reads it and removes it before rendering, so it is never in the output. A line has this form: `{%` or `{%-`, then `meta.<key>`, then `=` or `==`, then a value in double quotes or a whole number, then `%}` or `-%}`. Spaces between the parts and a CRLF line end are allowed. A `meta.` line lower in the file is not part of the block: MiniJinja reports it as a syntax error.

| Key | Form | Meaning |
| --- | --- | --- |
| `mode` | `= "0600"` | File mode. The same rules as `mode` in `[[template_rules]]`. |
| `uid` | `= 1001` | Owner, by numeric id. |
| `owner` | `= "name"` | Owner, by name. The same as `owner` in `[[template_rules]]`. |
| `gid` | `= 1001` | Group, by numeric id. |
| `group` | `= "name"` | Group, by name. |
| `os`, `arch`, `family`, `hostname`, `username` | `== "value"` | Condition. Compared with the [variable](#variables) of the same name. |

- **Settings** (`mode`, `uid`, `owner`, `gid`, `group`) make the template deploy as `template+copy` with that metadata. A template must not have both settings in its meta block and a `[[template_rules]]` entry. If it has both, rustow skips it with a warning: "set metadata either in the template meta block or in [[template_rules]], not both". A meta block with conditions only can have a rule.
- **Conditions** must all be true. If one is false, the template is not for this machine. rustow does not deploy it, prints no warning and keeps the exit code. `status` shows it as SKIPPED, for example "meta.os == "linux" is false here". A path argument that names it is not an error. `check` does not render it and does not test it for collisions.
- If the template was deployed on this machine before, for example before you changed its condition, its destination is ORPHANED with the false condition in the detail. rustow removes nothing. `rustow undeploy <source>` removes it.
- A key set twice, an unknown key, `=` on a condition, `==` on a setting, a wrong value type, `uid` with `owner` or `gid` with `group`: rustow skips the template with a warning that names the line, for example "template meta block, line 2: meta.mode is set twice". `rustow check` reports these mistakes, and settings together with a rule, as problems (exit 1), because `apply` would stop managing the template.
- Render errors count lines from the top of the file, meta lines included.
- Encrypted templates (`.tmpl.age`, `.tmpl.gpg`) have no meta block: rustow does not decrypt them to read it. Use `[[template_rules]]`. A meta line in an encrypted template is a render error.

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
