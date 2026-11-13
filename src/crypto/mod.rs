//! Encryption backends. Existing ciphertext is decrypted with the backend
//! named by its outer `.age` / `.gpg` suffix; `encryption.backend` (or
//! `rustow enc --backend`) only chooses the backend for new encryption.
//! Keys and the gpg executable load lazily, so sources that are ignored
//! or not selected never need them.

pub mod age;
pub mod gpg;

use std::cell::OnceCell;
use std::path::{Path, PathBuf};

use zeroize::Zeroizing;

use crate::config::Backend;
use crate::error::{Error, Result};
use crate::fsops;
use crate::hash::Hash;
use crate::paths::{RelPath, expand_tilde};
use crate::repo::Context;

type AgeIdentities = Vec<Box<dyn ::age::Identity>>;

/// Why decryption failed. Messages never contain plaintext.
#[derive(Debug, PartialEq, Eq)]
pub enum Failure {
    /// No private key here matches a recipient of the file.
    NoKey(String),
    /// A matching key is here, but it stays locked: no passphrase, or a wrong one.
    Locked(String),
    /// Not valid ciphertext, damaged, or another error.
    Corrupt(String),
}

impl Failure {
    pub fn message(self) -> String {
        match self {
            Failure::NoKey(m) | Failure::Locked(m) | Failure::Corrupt(m) => m,
        }
    }

    /// The same kind of failure, with the message that `f` makes of this one.
    pub fn map(self, f: impl FnOnce(String) -> String) -> Failure {
        match self {
            Failure::NoKey(m) => Failure::NoKey(f(m)),
            Failure::Locked(m) => Failure::Locked(f(m)),
            Failure::Corrupt(m) => Failure::Corrupt(f(m)),
        }
    }
}

/// Private keys and the gpg handle, loaded at most once per run.
#[derive(Default)]
pub struct Keys {
    age: OnceCell<std::result::Result<AgeIdentities, String>>,
    gpg: OnceCell<std::result::Result<gpg::Gpg, String>>,
}

/// Configured age identity files, `~` expanded. Relative paths are refused.
pub fn identity_paths(ctx: &Context) -> std::result::Result<Vec<PathBuf>, String> {
    let files = &ctx.config.encryption.identity_files;
    files
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let path = expand_tilde(f, &ctx.home);
            match path.is_absolute() {
                true => Ok(path),
                false => Err(format!(
                    "encryption.identity_files[{i}] must be absolute or start with ~/"
                )),
            }
        })
        .collect()
}

fn age_identities(ctx: &Context) -> std::result::Result<&[Box<dyn ::age::Identity>], String> {
    let loaded = ctx.keys.age.get_or_init(|| {
        let files = identity_paths(ctx)?;
        if files.is_empty() {
            return Err("no age identities configured (encryption.identity_files)".into());
        }
        for f in &files {
            age::check_identity_file(f, &[&ctx.root, &ctx.cache_dir])?;
        }
        let refs: Vec<&Path> = files.iter().map(PathBuf::as_path).collect();
        age::load_identities(&refs, ctx.interactive)
    });
    loaded.as_deref().map_err(Clone::clone)
}

/// GPG home: `encryption.gpg.homedir`, else `$GNUPGHOME`, else `~/.gnupg`.
pub fn gpg_homedir(ctx: &Context) -> std::result::Result<PathBuf, String> {
    // The error names where the relative path came from.
    let (home, relative) = match (&ctx.config.encryption.gpg.homedir, std::env::var_os("GNUPGHOME")) {
        (Some(dir), _) => (
            expand_tilde(dir, &ctx.home),
            "encryption.gpg.homedir must be absolute or start with ~/",
        ),
        (None, Some(env)) if !env.is_empty() => (PathBuf::from(env), "GNUPGHOME must be an absolute path"),
        // `ctx.home` is canonical.
        _ => return Ok(ctx.home.join(".gnupg")),
    };
    match home.is_absolute() {
        true => Ok(home),
        false => Err(relative.into()),
    }
}

fn gpg(ctx: &Context) -> std::result::Result<&gpg::Gpg, String> {
    let handle = ctx.keys.gpg.get_or_init(|| {
        let program = fsops::find_program("gpg")
            .ok_or("gpg not found in PATH; install GnuPG 2.2 or newer to use .gpg sources")?;
        let homedir = gpg_homedir(ctx)?;
        gpg::check_homedir(&homedir, &[&ctx.root, &ctx.cache_dir])?;
        Ok(gpg::Gpg {
            program,
            homedir,
            interactive: ctx.interactive,
        })
    });
    handle.as_ref().map_err(Clone::clone)
}

/// Authenticates and decrypts `ciphertext` of `source`. The message of an
/// error names the source and the cause, never content.
pub fn decrypt(
    backend: Backend,
    source: &RelPath,
    ciphertext: &[u8],
    ctx: &Context,
) -> std::result::Result<Zeroizing<Vec<u8>>, String> {
    open(backend, ciphertext, ctx).map_err(|f| format!("{source}: {}", f.message()))
}

/// Authenticates and decrypts `ciphertext` with the keys of this run.
fn open(backend: Backend, ciphertext: &[u8], ctx: &Context) -> std::result::Result<Zeroizing<Vec<u8>>, Failure> {
    match backend {
        Backend::Age => age::decrypt(
            ciphertext,
            age_identities(ctx).map_err(Failure::NoKey)?,
            ctx.interactive,
        ),
        Backend::Gpg => gpg(ctx).map_err(Failure::Corrupt)?.decrypt(ciphertext),
    }
}

/// Encrypts to every configured recipient of `backend`. A missing or
/// invalid recipient list is a configuration error.
pub fn encrypt(backend: Backend, plaintext: &[u8], ctx: &Context) -> Result<Vec<u8>> {
    match backend {
        Backend::Age => {
            let recipients = age::parse_recipients(&ctx.config.encryption.recipients).map_err(Error::Config)?;
            age::encrypt(plaintext, &recipients).map_err(Error::Failed)
        }
        Backend::Gpg => {
            let fingerprints: Vec<String> = gpg_recipients(ctx)?.into_iter().map(|(f, _)| f).collect();
            let gpg = gpg(ctx).map_err(Error::Failed)?;
            gpg.encrypt(plaintext, &fingerprints).map_err(Error::Failed)
        }
    }
}

/// Checks the configured recipients of `backend` as `encrypt` does, without encrypting.
pub fn check_recipients(backend: Backend, ctx: &Context) -> Result<()> {
    match backend {
        Backend::Age => age::parse_recipients(&ctx.config.encryption.recipients)
            .map(drop)
            .map_err(Error::Config),
        Backend::Gpg => gpg_recipients(ctx).map(drop),
    }
}

/// The configured GPG fingerprints, each resolved in the local keyring,
/// with the key ids that can encrypt for it.
fn gpg_recipients(ctx: &Context) -> Result<Vec<(String, Vec<String>)>> {
    let configured = &ctx.config.encryption.gpg.recipients;
    if configured.is_empty() {
        return Err(Error::Config(
            "encryption.gpg.recipients is empty; add full key fingerprints".into(),
        ));
    }
    let fingerprints = configured
        .iter()
        .enumerate()
        .map(|(i, f)| gpg::parse_fingerprint(f).map_err(|e| format!("encryption.gpg.recipients[{i}]: {e}")))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Error::Config)?;
    let gpg = gpg(ctx).map_err(Error::Failed)?;
    fingerprints
        .into_iter()
        .map(|f| gpg.resolve(&f).map(|ids| (f, ids)).map_err(Error::Failed))
        .collect()
}

/// Decrypts fresh ciphertext and compares it with `plaintext`.
/// `Ok(None)`: verified. `Ok(Some(why))`: no key here could check it.
/// `Err`: the ciphertext is broken or decrypts to other bytes.
pub fn verify(backend: Backend, ciphertext: &[u8], plaintext: &[u8], ctx: &Context) -> Result<Option<&'static str>> {
    match open(backend, ciphertext, ctx) {
        Ok(p) if *p == *plaintext => Ok(None),
        Ok(_) => Err(Error::Failed(
            "new ciphertext does not decrypt to the input; nothing changed".into(),
        )),
        Err(Failure::NoKey(_)) => Ok(Some("no matching private key available here")),
        Err(Failure::Locked(_)) if ctx.interactive => Ok(Some("the identity or key could not be unlocked")),
        Err(Failure::Locked(_)) => Ok(Some("the identity or key is locked; run with --interactive to verify")),
        Err(Failure::Corrupt(why)) => Err(Error::Failed(format!("new ciphertext failed verification: {why}"))),
    }
}

/// Identity of the configured recipient set of `backend`.
pub fn recipients_id(backend: Backend, ctx: &Context) -> Hash {
    let mut list: Vec<String> = match backend {
        Backend::Age => ctx
            .config
            .encryption
            .recipients
            .iter()
            .map(|r| r.trim().to_owned())
            .collect(),
        Backend::Gpg => ctx
            .config
            .encryption
            .gpg
            .recipients
            .iter()
            .map(|r| gpg::parse_fingerprint(r).unwrap_or_else(|_| r.clone()))
            .collect(),
    };
    list.sort();
    list.dedup();
    Hash::of(list.join("\n").as_bytes())
}

/// Whether ciphertext was evidently made for another recipient set, when
/// the format reveals it. age stanzas show only the number of recipients;
/// OpenPGP ENC_TO records show the key ids.
pub fn recipients_differ(backend: Backend, ciphertext: &[u8], ctx: &Context) -> bool {
    match backend {
        Backend::Age => age::stanza_count(ciphertext) != ctx.config.encryption.recipients.len(),
        // What gpg cannot tell is no evidence.
        Backend::Gpg => gpg_recipients_differ(ciphertext, ctx).unwrap_or_default(),
    }
}

/// True when an ENC_TO key id belongs to no configured key, or a
/// configured key has no ENC_TO key id. `None` when gpg cannot tell.
fn gpg_recipients_differ(ciphertext: &[u8], ctx: &Context) -> Option<bool> {
    let gpg = gpg(ctx).ok()?;
    let keys = gpg_recipients(ctx).ok()?;
    let enc_to = gpg.recipient_ids(ciphertext)?;
    let configured = |id: &String| keys.iter().any(|(_, ids)| ids.contains(id));
    let used = |ids: &Vec<String>| ids.iter().any(|id| enc_to.contains(id));
    Some(!enc_to.iter().all(configured) || !keys.iter().all(|(_, ids)| used(ids)))
}
