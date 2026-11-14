# Templates

A template is a source file whose name ends in the template extension (default `.tmpl`). rustow renders it with MiniJinja and deploys the result. The destination name drops the extension. `.gitconfig.tmpl` becomes `~/.gitconfig`.

## In short

- Name the file `something.tmpl`. Write `{{ vars.name }}` for your own values and `{% if os == "macos" %}…{% endif %}` for conditions.
- Set your values in `[vars]` in `rustow.toml`. The built-in variables are `os`, `hostname`, `username` and more (see [Variables](#variables)).
- An undefined variable is an error, so a typo never gives an empty value.
- Run `rustow check` to render the templates offline. Run `rustow diff` to see what would change.
- By default the result is a symlink to a cache file. To set the mode or the owner, use a `[[template_rules]]` entry or a [meta block](#meta-block). Then the result is a copy.

## Deployment

- Without a rule, a home template deploys as `template+link`. rustow writes the rendered file to the cache and links to it.
- With a `[[template_rules]]` entry, a template deploys as `template+copy`. The rule sets `mode`, `owner` and `group`.
- With settings in its [meta block](#meta-block), a template deploys as `template+copy` with those settings.
- A system template under `rustowroot/` always deploys as `template+copy`, also without a rule. See [system files](system-files.md).

```toml
[[template_rules]]
source = ".ssh/config.tmpl"
mode = "0600"
```

A new copy gets the rule `mode`. Without a rule `mode` (or without a rule, for a system template), a new copy gets 0644 (0600 if the source is encrypted), and an update keeps the mode of the existing file. The rule `mode` takes octal digits only and must include owner read (0400). Otherwise it is a configuration error (exit 2).

If you edited the cache entry through the link and then add a rule, `apply` reports a conflict on the cache entry. `--backup` saves your edit under `<state_dir>/backups/<repo-id>/<time>/cache/<dest>`.

## Meta block

A template can hold its own metadata and deploy conditions at the top of the file. Then `rustow.toml` needs no `[[template_rules]]` entry for it.

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

| Key                                            | Form         | Meaning                                                               |
| ---------------------------------------------- | ------------ | --------------------------------------------------------------------- |
| `mode`                                         | `= "0600"`   | File mode. The same rules as `mode` in `[[template_rules]]`.          |
| `uid`                                          | `= 1001`     | Owner, by numeric id.                                                 |
| `owner`                                        | `= "name"`   | Owner, by name. The same as `owner` in `[[template_rules]]`.          |
| `gid`                                          | `= 1001`     | Group, by numeric id.                                                 |
| `group`                                        | `= "name"`   | Group, by name.                                                       |
| `os`, `arch`, `family`, `hostname`, `username` | `== "value"` | Condition. Compared with the [variable](#variables) of the same name. |

- **Settings** (`mode`, `uid`, `owner`, `gid`, `group`) make the template deploy as `template+copy` with that metadata. A template must not have both settings in its meta block and a `[[template_rules]]` entry. If it has both, rustow skips it with a warning: "set metadata either in the template meta block or in [[template_rules]], not both". A meta block with conditions only can have a rule. Only root can give a home file to another user, or to a group that you are not in: run as another user, `apply` fails on such a template with "set owner of <path>: Operation not permitted", and `status` shows the same ERROR.
- **Conditions** must all be true. If one is false, the template is not for this machine. rustow does not deploy it, prints no warning and keeps the exit code. `status` shows it as SKIPPED, for example "meta.os == "linux" is false here". A path argument that names it is not an error. `check` does not render it and does not test it for collisions.
- If the template was deployed on this machine before, for example before you changed its condition, its destination is ORPHANED with the false condition in the detail. rustow removes nothing. `rustow undeploy <source>` removes it.
- A key set twice, an unknown key, `=` on a condition, `==` on a setting, a wrong value type, an id above 4294967294, `uid` with `owner` or `gid` with `group`: rustow skips the template with a warning that names the line, for example "template meta block, line 2: meta.mode is set twice". `rustow check` reports these mistakes, and settings together with a rule, as problems (exit 1), because `apply` would stop managing the template.
- An `owner` or `group` name that this system does not know is an error of that item in `apply` ("unknown user 'x'"). `rustow check` reports it as a problem: "<template>: template meta block: unknown user 'x'".
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

| Name         | Value                                                                           |
| ------------ | ------------------------------------------------------------------------------- |
| `os`         | `"linux"` or `"macos"`                                                          |
| `arch`       | CPU architecture                                                                |
| `family`     | `"unix"`                                                                        |
| `hostname`   | Host name                                                                       |
| `username`   | User name                                                                       |
| `uid`, `gid` | Numeric ids                                                                     |
| `home_dir`   | The home target                                                                 |
| `vars`       | Values from `[vars]`, after [`[[global_rules]]`](configuration.md#global_rules) |

## Rules of the engine

- An undefined variable is an error. This is true also in `if`. Use `{% if vars.x is defined %}`.
- rustow does not escape output automatically.
- A trailing newline stays.
- A template cannot include or import files.
- A template cannot run commands.
- Errors name the template and the line. They never show values. When MiniJinja's detail names only types, the error shows it, for example "template X, line N: invalid operation: tried to use + operator on unsupported types string and number". Other details can quote a value, so the error shows only the kind, for example "invalid operation".
- One render has a fixed evaluation limit of about 10 million steps (MiniJinja fuel). Real templates stay far below it. A template that goes over it, for example an endless or nested loop, fails with "template X, line N: exceeded the evaluation limit". Only that item fails: `apply` deploys the other items and exits with 4, `status` shows ERROR and `check` reports a problem. The limit bounds the time of a render, not the memory of the values that it makes.
- `apply` and `diff` plan every item before they write or show one, so they keep all rendered templates (and decrypted files) in memory at the same time. `status` keeps only the hash of each output. While it renders a template, the text and the output are in memory: a 200 MB template of plain text needs about 400 MB. MiniJinja also keeps a compiled form of each expression and of the text between them, so a template with many expressions needs more. With one expression in each line of 100 bytes, a 200 MB template needs about 900 MB.

## Commands

```sh
rustow template list          # list templates and their mode
rustow template apply         # deploy templates only
rustow check                  # renders plaintext templates
rustow diff                   # shows what would change
```

Encrypted templates (`name.tmpl.age`) are decrypted first, then rendered. `check` never decrypts them.

## Cache

A `template+link` goes to `<cache>/rendered/<repo-id>/<scope-id>/<dest>`, and the destination links to it. The directories have mode 0700 and the files 0600. A `template+copy` writes no cache entry: the rendered text goes straight to the destination. If you edit the deployed file through the symlink, you edit the cache entry. `status` shows the item as MODIFIED. `apply` reports a conflict: "cache entry: destination was modified since rustow last applied it". `--dry-run` writes no cache.

### When rustow renders again

rustow renders each selected template again on every run that plans it: `apply`, `template apply`, `status` and `diff`. The output changes when one of these changes:

- The template file. For an encrypted template, this is the decrypted text.
- A value in `[vars]`.
- A built-in variable, for example `hostname`, `uid` or `home_dir`.

rustow compares the hash of the new output with the cache entry. It writes the cache entry only when the content is different. Unchanged output is not rewritten. A `template+copy` destination follows the same rule: rustow compares the output with the destination and the state.

The cache path includes the repository id and the scope id. A moved repository, or another `--target` or `--root-target`, uses a new cache entry.
