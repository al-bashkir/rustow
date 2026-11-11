//! `rustow enc` encrypts a repository source in place; `rustow enc edit`
//! is the protected decrypt–edit–re-encrypt workflow. Neither deploys.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
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
use crate::state::{EncRecord, State};

/// Shown after every encryption: at-rest encryption is not erasure.
pub const HISTORY_NOTE: &str =
    "encryption does not remove earlier plaintext from Git history, backups or deployed files";

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
    check_plain_source(ctx, &rel, &abs)?;
    let backend = backend.unwrap_or(ctx.config.encryption.backend);
    let out_rel = rel.with_file_name(&format!("{}{}", rel.file_name(), backend.suffix()));
    let out = ctx.root.join(out_rel.as_path());
    if fs::symlink_metadata(&out).is_ok() {
        return Err(Error::Conflict(format!("{out_rel} already exists; nothing changed")));
    }
    let plaintext = Zeroizing::new(fsops::read_file(&abs)?);
    let ciphertext = crypto::encrypt(backend, &plaintext, ctx)?;
    let verified = crypto::verify(backend, &ciphertext, &plaintext, ctx)?;
    let mode = fs::metadata(&abs)
        .ctx(|| format!("inspect {rel}"))?
        .permissions()
        .mode()
        & 0o777;
    publish_new(&out, &ciphertext, mode)?;

    let config_path = ctx.root.join(CONFIG_FILE);
    let old_config = fs::read_to_string(&config_path).ok();
    let config_mode = fs::metadata(&config_path).map_or(0o644, |m| m.permissions().mode() & 0o7777);
    let migrated = match &old_config {
        Some(text) => edit::rename_source(text, &rel, &out_rel).inspect_err(|_| drop(fs::remove_file(&out)))?,
        None => None,
    };
    if let Some(text) = &migrated {
        fsops::write_atomic(&config_path, text.as_bytes(), config_mode, None, None)
            .inspect_err(|_| drop(fs::remove_file(&out)))?;
    }
    if fsops::read_file(&abs).map(|now| Hash::of(&now)).ok() != Some(Hash::of(&plaintext)) {
        if let (Some(_), Some(old)) = (&migrated, &old_config) {
            fsops::write_atomic(&config_path, old.as_bytes(), config_mode, None, None)?;
        }
        fs::remove_file(&out).ctx(|| format!("remove {out_rel}"))?;
        return Err(Error::Conflict(format!(
            "{rel} changed while it was being encrypted; nothing changed"
        )));
    }
    fs::remove_file(&abs).ctx(|| format!("remove plaintext {rel}"))?;
    let record = EncRecord {
        ciphertext_hash: Hash::of(&ciphertext),
        recipients_id: crypto::recipients_id(backend, ctx),
    };
    state.encryption.insert(out_rel.clone(), record);

    let mut detail = format!("-> {out_rel}; plaintext source removed");
    if migrated.is_some() {
        detail.push_str("; registrations moved");
    }
    if !verified {
        detail.push_str("; not verified: no matching private key available here");
    }
    printer.changes(None, &[Change::new(rel.as_str(), "encrypt", Some(detail))], false);
    printer.notice(HISTORY_NOTE);
    Ok(0)
}

fn check_plain_source(ctx: &Context, rel: &RelPath, abs: &Path) -> Result<()> {
    let usage = |why: &str| Err(Error::Usage(format!("'{rel}' {why}")));
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

/// Writes a new file without ever replacing an existing one.
fn publish_new(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| Error::Failed(format!("{} has no parent", path.display())))?;
    let what = || format!("write {}", path.display());
    let mut tmp = tempfile::Builder::new().prefix(".rustow-").tempfile_in(dir).ctx(what)?;
    tmp.write_all(bytes).ctx(what)?;
    tmp.as_file()
        .set_permissions(fs::Permissions::from_mode(mode))
        .ctx(what)?;
    tmp.as_file().sync_all().ctx(what)?;
    tmp.persist_noclobber(path).map_err(|e| e.error).ctx(what)?;
    Ok(())
}

/// Decrypts `arg` into a private workspace, runs the editor, and
/// re-encrypts with the source's own backend when content or recipients changed.
pub fn edit_source(ctx: &Context, state: &mut State, arg: &str, reencrypt: bool, printer: &Printer) -> Result<i32> {
    let rel = RelPath::parse(arg)?;
    let backend = suffix_backend(&rel)
        .ok_or_else(|| Error::Usage(format!("'{rel}' is not an encrypted source (*.age or *.gpg)")))?;
    let abs = ctx.root.join(rel.as_path());
    if !fs::symlink_metadata(&abs).is_ok_and(|m| m.is_file()) {
        return Err(Error::Usage(format!("'{rel}' is not a regular file in the repository")));
    }
    let original = fsops::read_file(&abs)?;
    let original_hash = Hash::of(&original);
    let plaintext = crypto::decrypt(backend, &rel, &original, ctx).map_err(Error::Failed)?;
    let workspace = Workspace::create(ctx, &rel, backend, &plaintext)?;
    let status = run_editor(&workspace.file)?;
    if !status.success() {
        return Err(Error::Failed(format!(
            "editor exited with {status}; {rel} is unchanged"
        )));
    }
    let edited = Zeroizing::new(fs::read(&workspace.file).ctx(|| "read the edited file".into())?);
    let unchanged_source = || fsops::read_file(&abs).is_ok_and(|now| Hash::of(&now) == original_hash);
    if !unchanged_source() {
        return Err(conflict(&rel, workspace.keep()));
    }
    let recipients = crypto::recipients_id(backend, ctx);
    let recipients_changed = match state.encryption.get(&rel) {
        Some(r) if r.ciphertext_hash == original_hash => r.recipients_id != recipients,
        _ => crypto::recipients_differ(backend, &original, ctx),
    };
    if *edited == *plaintext && !recipients_changed && !reencrypt {
        printer.changes(
            None,
            &[Change::new(
                rel.as_str(),
                "ok",
                Some("unchanged; ciphertext not rewritten".into()),
            )],
            false,
        );
        return Ok(0);
    }
    let ciphertext = match crypto::encrypt(backend, &edited, ctx).and_then(|c| verified(backend, c, &edited, ctx)) {
        Ok(c) => c,
        Err(e) => {
            let kept = workspace.keep();
            return Err(Error::Failed(format!(
                "{e}; {rel} is unchanged; your edit is kept in {}",
                kept.display()
            )));
        }
    };
    if !unchanged_source() {
        return Err(conflict(&rel, workspace.keep()));
    }
    let mode = fs::metadata(&abs)
        .ctx(|| format!("inspect {rel}"))?
        .permissions()
        .mode()
        & 0o777;
    fsops::write_atomic(&abs, &ciphertext, mode, None, None)?;
    state.encryption.insert(
        rel.clone(),
        EncRecord {
            ciphertext_hash: Hash::of(&ciphertext),
            recipients_id: recipients,
        },
    );
    let detail = "re-encrypted; deployed files change on the next `rustow apply`".to_owned();
    printer.changes(None, &[Change::new(rel.as_str(), "update", Some(detail))], false);
    Ok(0)
}

fn verified(backend: Backend, ciphertext: Vec<u8>, plaintext: &[u8], ctx: &Context) -> Result<Vec<u8>> {
    crypto::verify(backend, &ciphertext, plaintext, ctx)?;
    Ok(ciphertext)
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

/// Runs `$VISUAL`, else `$EDITOR`, else `vi`, on `file`. The command is
/// split on whitespace and never passed to a shell. While it runs, the
/// first SIGINT/SIGTERM/SIGHUP does not kill rustow, so it can clean up.
fn run_editor(file: &Path) -> Result<std::process::ExitStatus> {
    let var = |name| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    let editor = var("VISUAL").or_else(|| var("EDITOR")).unwrap_or_else(|| "vi".into());
    let mut words = editor.split_whitespace();
    let program = words.next().unwrap_or("vi");
    let stop = Arc::new(AtomicBool::new(false));
    for sig in [
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
    ] {
        signal_hook::flag::register(sig, Arc::clone(&stop))
            .map_err(|e| Error::Failed(format!("signal handler: {e}")))?;
    }
    let status = Command::new(program)
        .args(words)
        .arg(file)
        .status()
        .map_err(|e| Error::Failed(format!("cannot run editor '{program}': {e}")))?;
    if stop.load(Ordering::SeqCst) && !status.success() {
        return Err(Error::Failed("interrupted; the encrypted source is unchanged".into()));
    }
    Ok(status)
}
