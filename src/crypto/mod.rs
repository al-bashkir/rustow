//! Encryption backends. Existing ciphertext is decrypted with the backend
//! named by its outer `.age` / `.gpg` suffix; `encryption.backend` (or
//! `rustow enc --backend`) only chooses the backend for new encryption.
//! Private keys load lazily, so sources that are ignored or not selected
//! never need them.

pub mod age;

use std::cell::OnceCell;
use std::path::PathBuf;

use zeroize::Zeroizing;

use crate::config::Backend;
use crate::error::{Error, Result};
use crate::hash::Hash;
use crate::paths::{RelPath, expand_tilde};
use crate::repo::Context;

type AgeIdentities = Vec<Box<dyn ::age::Identity>>;

/// Private keys, loaded at most once per run.
#[derive(Default)]
pub struct Keys {
    age: OnceCell<std::result::Result<AgeIdentities, String>>,
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
        let refs: Vec<&std::path::Path> = files.iter().map(PathBuf::as_path).collect();
        age::load_identities(&refs, ctx.interactive)
    });
    loaded.as_deref().map_err(Clone::clone)
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
        Backend::Gpg => Err(format!("{source}: GPG decryption is not available yet")),
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
        Backend::Gpg => Err(Error::Failed("GPG encryption is not available yet".into())),
    }
}

/// Decrypts fresh ciphertext and compares it with `plaintext` when a
/// matching private key is available. `Ok(false)`: nothing to verify with.
pub fn verify(backend: Backend, ciphertext: &[u8], plaintext: &[u8], ctx: &Context) -> Result<bool> {
    match backend {
        Backend::Age => match age_identities(ctx) {
            Err(_) => Ok(false),
            Ok(ids) => match age::decrypt(ciphertext, ids) {
                Ok(p) if *p == *plaintext => Ok(true),
                Ok(_) => Err(Error::Failed(
                    "new ciphertext does not decrypt to the input; nothing changed".into(),
                )),
                Err(age::Failure::NoMatchingKey) => Ok(false),
                Err(age::Failure::Corrupt(why)) => {
                    Err(Error::Failed(format!("new ciphertext failed verification: {why}")))
                }
            },
        },
        Backend::Gpg => Ok(false),
    }
}

/// Identity of the configured recipient set of `backend`.
pub fn recipients_id(backend: Backend, ctx: &Context) -> Hash {
    let mut list = match backend {
        Backend::Age => ctx.config.encryption.recipients.clone(),
        Backend::Gpg => ctx.config.encryption.gpg.recipients.clone(),
    };
    list.iter_mut().for_each(|r| *r = r.trim().to_ascii_lowercase());
    list.sort();
    list.dedup();
    Hash::of(list.join("\n").as_bytes())
}

/// Whether ciphertext was evidently made for another recipient set, when
/// the format reveals it (age: number of stanzas).
pub fn recipients_differ(backend: Backend, ciphertext: &[u8], ctx: &Context) -> bool {
    match backend {
        Backend::Age => age::stanza_count(ciphertext) != ctx.config.encryption.recipients.len(),
        Backend::Gpg => false,
    }
}
