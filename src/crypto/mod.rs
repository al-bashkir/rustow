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
    let home = match (&ctx.config.encryption.gpg.homedir, std::env::var_os("GNUPGHOME")) {
        (Some(dir), _) => expand_tilde(dir, &ctx.home),
        (None, Some(env)) if !env.is_empty() => PathBuf::from(env),
        _ => ctx.home.join(".gnupg"),
    };
    match home.is_absolute() {
        true => Ok(home),
        false => Err("encryption.gpg.homedir must be absolute or start with ~/".into()),
    }
}

fn gpg(ctx: &Context) -> std::result::Result<&gpg::Gpg, String> {
    let handle = ctx.keys.gpg.get_or_init(|| {
        let program = fsops::find_in_path("gpg")
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
    match backend {
        Backend::Age => {
            let identities = age_identities(ctx).map_err(|e| format!("{source}: {e}"))?;
            age::decrypt(ciphertext, identities).map_err(|f| match f {
                age::Failure::NoMatchingKey => format!("{source}: no configured age identity can decrypt it"),
                age::Failure::Corrupt(why) => format!("{source}: {why}"),
            })
        }
        Backend::Gpg => gpg(ctx)
            .and_then(|g| g.decrypt(ciphertext))
            .map_err(|e| format!("{source}: {e}")),
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
            for f in &fingerprints {
                gpg.resolve(f).map_err(Error::Failed)?;
            }
            gpg.encrypt(plaintext, &fingerprints).map_err(Error::Failed)
        }
    }
}

/// Decrypts fresh ciphertext and compares it with `plaintext` when a
/// matching private key is available. `Ok(false)`: nothing to verify with.
pub fn verify(backend: Backend, ciphertext: &[u8], plaintext: &[u8], ctx: &Context) -> Result<bool> {
    let mismatch = || Error::Failed("new ciphertext does not decrypt to the input; nothing changed".into());
    match backend {
        Backend::Age => match age_identities(ctx) {
            Err(_) => Ok(false),
            Ok(ids) => match age::decrypt(ciphertext, ids) {
                Ok(p) if *p == *plaintext => Ok(true),
                Ok(_) => Err(mismatch()),
                Err(age::Failure::NoMatchingKey) => Ok(false),
                Err(age::Failure::Corrupt(why)) => {
                    Err(Error::Failed(format!("new ciphertext failed verification: {why}")))
                }
            },
        },
        // A locked or absent secret key only means "not verified"; the
        // status records already confirmed encryption completed.
        Backend::Gpg => match gpg(ctx).and_then(|g| g.decrypt(ciphertext)) {
            Ok(p) if *p == *plaintext => Ok(true),
            Ok(_) => Err(mismatch()),
            Err(_) => Ok(false),
        },
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
/// the format reveals it (number of age stanzas or OpenPGP ENC_TO records).
pub fn recipients_differ(backend: Backend, ciphertext: &[u8], ctx: &Context) -> bool {
    match backend {
        Backend::Age => age::stanza_count(ciphertext) != ctx.config.encryption.recipients.len(),
        Backend::Gpg => gpg(ctx)
            .ok()
            .and_then(|g| g.recipient_count(ciphertext))
            .is_some_and(|n| n != ctx.config.encryption.gpg.recipients.len()),
    }
}
