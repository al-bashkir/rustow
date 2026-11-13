//! `rustow enc` encrypts a repository source in place; `rustow enc edit`
//! is the protected decrypt–edit–re-encrypt workflow. Neither deploys.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use zeroize::Zeroizing;

use crate::config::{Backend, CONFIG_FILE, edit};
use crate::crypto;
use crate::error::{Error, IoContext, Result};
use crate::fsops;
use crate::hash::Hash;
use crate::paths::RelPath;
use crate::repo::Context;
use crate::report::{Change, Printer};
use crate::source;
use crate::state::{EncRecord, State};

/// Shown after every encryption: at-rest encryption is not erasure.
pub const HISTORY_NOTE: &str =
    "encryption does not remove earlier plaintext from Git history, backups or deployed files";

/// An encrypted copy of a source, ready to be published.
struct Sealed {
    backend: Backend,
    out_rel: RelPath,
    out: PathBuf,
    plaintext: Zeroizing<Vec<u8>>,
    ciphertext: Vec<u8>,
    /// Why no private key here could check the result (`crypto::verify`).
    unverified: Option<&'static str>,
    mode: u32,
}

/// The configuration file before and after moving registrations to the encrypted name.
struct ConfigMigration {
    path: PathBuf,
    mode: u32,
    old: Option<String>,
    new: Option<String>,
}

impl ConfigMigration {
    /// Puts the old configuration back when it was rewritten.
    fn rollback(&self) -> Result<()> {
        if let (Some(_), Some(old)) = (&self.new, &self.old) {
            fsops::write_atomic(&self.path, old.as_bytes(), self.mode, None, None)?;
        }
        Ok(())
    }
}

/// Encrypts `arg` to `<arg>.age` or `<arg>.gpg`, migrates registrations,
/// then removes the plaintext source.
pub fn encrypt_source(
    ctx: &Context,
    state: &mut State,
    arg: &str,
    backend: Option<Backend>,
    printer: &Printer,
) -> Result<i32> {
    let rel = RelPath::parse(arg)?;
    let abs = ctx.root.join(rel.as_path());
    let sealed = seal_source(ctx, &rel, &abs, backend)?;
    fsops::write_new(&sealed.out, &sealed.ciphertext, sealed.mode)?;
    let migration = migrate_config(ctx, &rel, &sealed)?;
    retire_plaintext(&rel, &abs, &sealed, &migration)?;
    let mut migrated = migration.new.is_some();
    for registered in [&mut state.files, &mut state.links] {
        if registered.remove(&rel) {
            registered.insert(sealed.out_rel.clone());
            migrated = true;
        }
    }
    state.encryption.insert(
        sealed.out_rel.clone(),
        EncRecord {
            ciphertext_hash: Hash::of(&sealed.ciphertext),
            recipients_id: crypto::recipients_id(sealed.backend, ctx),
        },
    );
    report_encrypted(&rel, &sealed, migrated, printer);
    Ok(0)
}

/// Validates the source and encrypts it (without writing anything).
fn seal_source(ctx: &Context, rel: &RelPath, abs: &Path, backend: Option<Backend>) -> Result<Sealed> {
    check_plain_source(ctx, rel, abs)?;
    let backend = backend.unwrap_or(ctx.config.encryption.backend);
    let out_rel = rel.with_file_name(&format!("{}{}", rel.file_name(), backend.suffix()));
    let out = ctx.root.join(out_rel.as_path());
    if fs::symlink_metadata(&out).is_ok() {
        return Err(Error::Conflict(format!("{out_rel} already exists; nothing changed")));
    }
    let plaintext = Zeroizing::new(fsops::read_file(abs)?);
    let ciphertext = crypto::encrypt(backend, &plaintext, ctx)?;
    let unverified = crypto::verify(backend, &ciphertext, &plaintext, ctx)?;
    let mode = file_mode(abs, rel)?;
    Ok(Sealed {
        backend,
        out_rel,
        out,
        plaintext,
        ciphertext,
        unverified,
        mode,
    })
}

/// Permission bits of the file at `abs`.
fn file_mode(abs: &Path, rel: &RelPath) -> Result<u32> {
    let meta = fs::metadata(abs).ctx(|| format!("inspect {rel}"))?;
    Ok(meta.permissions().mode() & 0o777)
}

/// Moves registrations of `rel` to the encrypted name; removes the new file when that fails.
fn migrate_config(ctx: &Context, rel: &RelPath, sealed: &Sealed) -> Result<ConfigMigration> {
    let path = ctx.root.join(CONFIG_FILE);
    let old = fs::read_to_string(&path).ok();
    let mode = fs::metadata(&path).map_or(0o644, |m| m.permissions().mode() & 0o7777);
    let discard = |_: &Error| drop(fs::remove_file(&sealed.out));
    let new = match &old {
        Some(text) => edit::rename_source(text, rel, &sealed.out_rel).inspect_err(discard)?,
        None => None,
    };
    if let Some(text) = &new {
        fsops::write_atomic(&path, text.as_bytes(), mode, None, None).inspect_err(discard)?;
    }
    Ok(ConfigMigration { path, mode, old, new })
}

/// Removes the plaintext, unless it changed meanwhile: then undo everything
/// and fail. The plaintext first loses its name, so a writer that opens the
/// path later makes a new file there instead of changing the checked content.
// shortcut: a writer that already holds the plaintext open can still append after the check; closing that needs a file lease, which no portable API offers.
fn retire_plaintext(rel: &RelPath, abs: &Path, sealed: &Sealed, migration: &ConfigMigration) -> Result<()> {
    let aside = set_aside(abs).ctx(|| format!("set plaintext {rel} aside"))?;
    if fsops::read_file(&aside).map(|now| Hash::of(&now)).ok() == Some(Hash::of(&sealed.plaintext)) {
        return aside.close().ctx(|| format!("remove plaintext {rel}"));
    }
    migration.rollback()?;
    fs::remove_file(&sealed.out).ctx(|| format!("remove {}", sealed.out_rel))?;
    let kept = aside
        .keep()
        .map_err(|e| e.error)
        .ctx(|| format!("keep plaintext {rel}"))?;
    let outcome = match fs::hard_link(&kept, abs).and_then(|()| fs::remove_file(&kept)) {
        Ok(()) => "nothing changed".to_string(),
        Err(_) => format!("encryption undone; the plaintext it read is kept at {}", kept.display()),
    };
    Err(Error::Conflict(format!(
        "{rel} changed while it was being encrypted; {outcome}"
    )))
}

/// Gives `abs` a second, reserved name in its directory, then removes `abs`.
/// The returned path removes that name when dropped.
fn set_aside(abs: &Path) -> std::io::Result<tempfile::TempPath> {
    let dir = abs.parent().unwrap_or(Path::new("."));
    let aside = tempfile::Builder::new()
        .prefix(fsops::TEMP_PREFIX)
        .make_in(dir, |p| fs::hard_link(abs, p))?
        .into_temp_path();
    fs::remove_file(abs)?;
    Ok(aside)
}

fn report_encrypted(rel: &RelPath, sealed: &Sealed, migrated: bool, printer: &Printer) {
    let mut detail = format!("-> {}; plaintext source removed", sealed.out_rel);
    if migrated {
        detail.push_str("; registrations moved");
    }
    detail.push_str(&not_verified(sealed.unverified));
    printer.changes(None, &[Change::new(rel.as_str(), "encrypt", Some(detail))], false);
    printer.notice(HISTORY_NOTE);
}

/// The note for a result that no private key here could check.
fn not_verified(why: Option<&str>) -> String {
    why.map(|why| format!("; not verified: {why}")).unwrap_or_default()
}

/// Checks each parent directory of `rel` as discovery does, so that no
/// symlink leads out of the repository.
fn check_parents(ctx: &Context, rel: &RelPath) -> Result<()> {
    let text = rel.as_str();
    for (end, _) in text.match_indices('/') {
        let parent = &text[..end];
        let meta = fs::symlink_metadata(ctx.root.join(parent));
        if let Some(what) = meta.ok().and_then(|m| source::unexpected_kind(m.file_type())) {
            return Err(Error::Usage(format!(
                "source '{parent}' is {what}; rustow encrypts only files inside the repository; use the real path"
            )));
        }
    }
    Ok(())
}

fn check_plain_source(ctx: &Context, rel: &RelPath, abs: &Path) -> Result<()> {
    let usage = |why: &str| Err(Error::Usage(format!("'{rel}' {why}")));
    check_parents(ctx, rel)?;
    if ctx.ignore.is_reserved(rel, abs.is_dir()) {
        return usage("is reserved by rustow and cannot be encrypted");
    }
    match fs::symlink_metadata(abs) {
        Err(_) => return usage("does not exist"),
        Ok(m) if m.file_type().is_symlink() => return usage("is a symlink; encrypt the file it points to instead"),
        Ok(m) if !m.is_file() => return usage("is not a regular file"),
        Ok(_) => {}
    }
    if suffix_backend(rel).is_some() {
        return usage("is already encrypted; use `rustow enc edit`");
    }
    Ok(())
}

fn suffix_backend(rel: &RelPath) -> Option<Backend> {
    [Backend::Age, Backend::Gpg]
        .into_iter()
        .find(|b| rel.file_name().ends_with(b.suffix()))
}

/// An encrypted source opened for editing.
struct Opened {
    rel: RelPath,
    backend: Backend,
    abs: PathBuf,
    original: Vec<u8>,
    original_hash: Hash,
    plaintext: Zeroizing<Vec<u8>>,
}

impl Opened {
    /// True while the encrypted file is the one that was opened.
    fn unchanged(&self) -> bool {
        fsops::read_file(&self.abs).is_ok_and(|now| Hash::of(&now) == self.original_hash)
    }
}

/// Decrypts `arg` into a private workspace, runs the editor, and
/// re-encrypts with the source's own backend when content or recipients changed.
pub fn edit_source(ctx: &Context, state: &mut State, arg: &str, reencrypt: bool, printer: &Printer) -> Result<i32> {
    let src = open_encrypted(ctx, arg)?;
    // Before plaintext is on disk: a first signal cancels the edit, so the workspace goes.
    let stop = crate::app::interrupt_flag()?;
    let workspace = Workspace::create(ctx, &src.rel, src.backend, &src.plaintext)?;
    let edited = edit_in_workspace(&src.rel, &workspace, &stop)?;
    if !src.unchanged() {
        return Err(conflict(&src.rel, workspace.keep()));
    }
    let recipients = crypto::recipients_id(src.backend, ctx);
    if !needs_rewrite(ctx, state, &src, &edited, reencrypt, &recipients) {
        let detail = Some("unchanged; ciphertext not rewritten".into());
        printer.changes(None, &[Change::new(src.rel.as_str(), "ok", detail)], false);
        return Ok(0);
    }
    let (ciphertext, unverified) = encrypt_edited(ctx, &src, &edited, workspace)?;
    interrupted(&stop, &src.rel)?;
    write_back(state, &src, &ciphertext, recipients)?;
    let detail = format!(
        "re-encrypted; deployed files change on the next `rustow apply`{}",
        not_verified(unverified)
    );
    printer.changes(None, &[Change::new(src.rel.as_str(), "update", Some(detail))], false);
    Ok(0)
}

/// Checks that `arg` is an encrypted source file and decrypts it.
fn open_encrypted(ctx: &Context, arg: &str) -> Result<Opened> {
    let rel = RelPath::parse(arg)?;
    let (backend, _, _) = source::split_suffixes(rel.file_name(), &ctx.config.template_extension)
        .map_err(|why| Error::Usage(format!("'{rel}': {why}")))?;
    let backend =
        backend.ok_or_else(|| Error::Usage(format!("'{rel}' is not an encrypted source (*.age or *.gpg)")))?;
    let abs = ctx.root.join(rel.as_path());
    check_parents(ctx, &rel)?;
    if !fs::symlink_metadata(&abs).is_ok_and(|m| m.is_file()) {
        return Err(Error::Usage(format!("'{rel}' is not a regular file in the repository")));
    }
    // Fail before any plaintext exists when the edit could not be encrypted again.
    crypto::check_recipients(backend, ctx)?;
    let original = fsops::read_file(&abs)?;
    let original_hash = Hash::of(&original);
    let plaintext = crypto::decrypt(backend, &rel, &original, ctx).map_err(Error::Failed)?;
    Ok(Opened {
        rel,
        backend,
        abs,
        original,
        original_hash,
        plaintext,
    })
}

/// Runs the editor on the workspace file and reads the result.
fn edit_in_workspace(rel: &RelPath, workspace: &Workspace, stop: &AtomicBool) -> Result<Zeroizing<Vec<u8>>> {
    let status = run_editor(&workspace.file)?;
    interrupted(stop, rel)?;
    if !status.success() {
        return Err(Error::Failed(format!(
            "editor exited with {status}; {rel} is unchanged"
        )));
    }
    Ok(Zeroizing::new(
        fs::read(&workspace.file).ctx(|| "read the edited file".into())?,
    ))
}

/// True when the content or the recipients changed, or a rewrite was asked for.
fn needs_rewrite(
    ctx: &Context,
    state: &State,
    src: &Opened,
    edited: &[u8],
    reencrypt: bool,
    recipients: &Hash,
) -> bool {
    *edited != **src.plaintext || reencrypt || recipients_changed(ctx, state, src, recipients)
}

/// True when the recipient set differs from the one the file was encrypted to.
fn recipients_changed(ctx: &Context, state: &State, src: &Opened, recipients: &Hash) -> bool {
    match state.encryption.get(&src.rel) {
        Some(r) if r.ciphertext_hash == src.original_hash => r.recipients_id != *recipients,
        _ => crypto::recipients_differ(src.backend, &src.original, ctx),
    }
}

/// Encrypts and verifies the edit (with the reason it is not verified).
/// On failure or a concurrent change, the workspace is kept.
fn encrypt_edited(
    ctx: &Context,
    src: &Opened,
    edited: &[u8],
    workspace: Workspace,
) -> Result<(Vec<u8>, Option<&'static str>)> {
    let encrypted = crypto::encrypt(src.backend, edited, ctx)
        .and_then(|c| crypto::verify(src.backend, &c, edited, ctx).map(|why| (c, why)));
    let sealed = match encrypted {
        Ok(sealed) => sealed,
        Err(e) => {
            let kept = workspace.keep();
            return Err(Error::Failed(format!(
                "{e}; {} is unchanged; your edit is kept in {}",
                src.rel,
                kept.display()
            )));
        }
    };
    if !src.unchanged() {
        return Err(conflict(&src.rel, workspace.keep()));
    }
    Ok(sealed)
}

/// Replaces the encrypted source and records the new ciphertext.
fn write_back(state: &mut State, src: &Opened, ciphertext: &[u8], recipients: Hash) -> Result<()> {
    let mode = file_mode(&src.abs, &src.rel)?;
    fsops::write_atomic(&src.abs, ciphertext, mode, None, None)?;
    state.encryption.insert(
        src.rel.clone(),
        EncRecord {
            ciphertext_hash: Hash::of(ciphertext),
            recipients_id: recipients,
        },
    );
    Ok(())
}

fn conflict(rel: &RelPath, kept: PathBuf) -> Error {
    Error::Conflict(format!(
        "{rel} changed while you were editing; not overwritten. Your edited plaintext is kept in {} (`rustow cache clean` removes it)",
        kept.display()
    ))
}

/// A private directory under `<cache>/edit/` holding one plaintext file.
/// Removed on drop unless kept; a held lock marks it as in use.
struct Workspace {
    dir: Option<tempfile::TempDir>,
    file: PathBuf,
    _lock: File,
}

impl Workspace {
    fn create(ctx: &Context, rel: &RelPath, backend: Backend, plaintext: &[u8]) -> Result<Workspace> {
        let root = ctx.cache_dir.join("edit");
        fsops::ensure_private_dir(&ctx.cache_dir)?;
        fsops::ensure_private_dir(&root)?;
        let dir = tempfile::Builder::new()
            .prefix("edit-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(&root)
            .ctx(|| format!("create workspace in {}", root.display()))?;
        let lock = File::create(dir.path().join(".lock")).ctx(|| "create workspace lock".into())?;
        lock.try_lock()
            .map_err(|e| Error::Failed(format!("lock workspace: {e}")))?;
        let name = rel
            .file_name()
            .strip_suffix(backend.suffix())
            .unwrap_or(rel.file_name());
        let file = dir.path().join(name);
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&file)
            .ctx(|| format!("create {}", file.display()))?;
        f.write_all(plaintext).ctx(|| format!("write {}", file.display()))?;
        Ok(Workspace {
            dir: Some(dir),
            file,
            _lock: lock,
        })
    }

    /// Keeps the workspace on disk and returns the edited file's path.
    fn keep(mut self) -> PathBuf {
        if let Some(dir) = self.dir.take() {
            let _ = dir.keep();
        }
        self.file.clone()
    }
}

/// Fails when a signal asked rustow to stop: the edit is not saved.
fn interrupted(stop: &AtomicBool, rel: &RelPath) -> Result<()> {
    match stop.load(Ordering::SeqCst) {
        true => Err(Error::Failed(format!(
            "interrupted; the edit is not saved and {rel} is unchanged"
        ))),
        false => Ok(()),
    }
}

/// Runs `$VISUAL`, else `$EDITOR`, else `vi`, on `file`. The command is
/// split on whitespace and never passed to a shell.
fn run_editor(file: &Path) -> Result<std::process::ExitStatus> {
    let var = |name| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    let editor = var("VISUAL").or_else(|| var("EDITOR")).unwrap_or_else(|| "vi".into());
    let mut words = editor.split_whitespace();
    let program = words.next().unwrap_or("vi");
    let path = crate::fsops::find_program(program)
        .ok_or_else(|| Error::Failed(format!("cannot run editor '{program}': not found in PATH")))?;
    Command::new(path)
        .args(words)
        .arg(file)
        .status()
        .map_err(|e| Error::Failed(format!("cannot run editor '{program}': {e}")))
}
