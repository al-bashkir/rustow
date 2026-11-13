//! age backend (Rust-native `age` crate, X25519 recipients only).

use std::io::{BufRead, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::str::FromStr;

use age::armor::ArmoredReader;
use age::secrecy::SecretString;
use age::{Callbacks, DecryptError, Identity, Recipient, x25519};
use zeroize::Zeroizing;

use super::Failure;

pub fn parse_recipients(list: &[String]) -> Result<Vec<x25519::Recipient>, String> {
    if list.is_empty() {
        return Err("encryption.recipients is empty; add age public keys (age1...)".into());
    }
    list.iter()
        .enumerate()
        .map(|(i, r)| {
            x25519::Recipient::from_str(r.trim())
                .map_err(|e| format!("encryption.recipients[{i}] is not an age X25519 public key: {e}"))
        })
        .collect()
}

pub fn encrypt(plaintext: &[u8], recipients: &[x25519::Recipient]) -> Result<Vec<u8>, String> {
    let fail = |e: &dyn std::fmt::Display| format!("age encryption failed: {e}");
    let encryptor =
        age::Encryptor::with_recipients(recipients.iter().map(|r| r as &dyn Recipient)).map_err(|e| fail(&e))?;
    let mut out = Vec::with_capacity(plaintext.len() + 256);
    let mut writer = encryptor.wrap_output(&mut out).map_err(|e| fail(&e))?;
    writer.write_all(plaintext).map_err(|e| fail(&e))?;
    writer.finish().map_err(|e| fail(&e))?;
    Ok(out)
}

/// Decrypts and authenticates the whole input (binary or armored) before
/// returning anything.
pub fn decrypt(ciphertext: &[u8], identities: &[Box<dyn Identity>]) -> Result<Zeroizing<Vec<u8>>, Failure> {
    let decryptor = age::Decryptor::new_buffered(ArmoredReader::new(ciphertext)).map_err(failure)?;
    if decryptor.is_scrypt() {
        return Err(Failure::Corrupt(
            "passphrase-only (scrypt) age files are not supported; encrypt to recipients".into(),
        ));
    }
    let mut reader = decryptor
        .decrypt(identities.iter().map(|i| i.as_ref()))
        .map_err(failure)?;
    let mut plaintext = Zeroizing::new(Vec::with_capacity(ciphertext.len()));
    reader
        .read_to_end(&mut plaintext)
        .map_err(|e| Failure::Corrupt(format!("authentication failed ({e}); the file is damaged or truncated")))?;
    Ok(plaintext)
}

fn failure(e: DecryptError) -> Failure {
    match e {
        DecryptError::NoMatchingKeys => Failure::NoKey("no configured age identity can decrypt it".into()),
        // An encrypted identity file without a passphrase, or with a wrong one.
        DecryptError::KeyDecryptionFailed => Failure::Locked(
            "could not unlock a passphrase-protected identity file (wrong passphrase, or rerun with --interactive)"
                .into(),
        ),
        other => Failure::Corrupt(format!("not valid age ciphertext: {other}")),
    }
}

/// Number of X25519 recipient stanzas in an age header (binary or armored).
pub fn stanza_count(ciphertext: &[u8]) -> usize {
    ArmoredReader::new(ciphertext)
        .split(b'\n')
        .map_while(std::io::Result::ok)
        .take_while(|line| !line.starts_with(b"---"))
        .filter(|line| line.starts_with(b"-> X25519 "))
        .count()
}

/// An identity file must be a regular file of the current user, private
/// (no group/other access), and outside the repository and cache.
pub fn check_identity_file(path: &Path, forbidden: &[&Path]) -> Result<(), String> {
    let shown = path.display();
    let meta = std::fs::metadata(path).map_err(|e| format!("age identity file {shown}: {e}"))?;
    let real = path
        .canonicalize()
        .map_err(|e| format!("age identity file {shown}: {e}"))?;
    if let Some(dir) = forbidden.iter().find(|d| real.starts_with(d)) {
        return Err(format!(
            "age identity file {shown} is inside {}; keep private keys outside the repository",
            dir.display()
        ));
    }
    if !meta.is_file() {
        return Err(format!("age identity file {shown} is not a regular file"));
    }
    if meta.uid() != nix::unistd::geteuid().as_raw() {
        return Err(format!(
            "age identity file {shown} is owned by uid {}, not by you",
            meta.uid()
        ));
    }
    if meta.permissions().mode() & 0o077 != 0 {
        return Err(format!(
            "age identity file {shown} is accessible by group or others; run chmod 600 {shown}"
        ));
    }
    Ok(())
}

/// Loads identities from checked files. Passphrase-protected identity
/// files prompt on the terminal only when `interactive`.
pub fn load_identities(files: &[&Path], interactive: bool) -> Result<Vec<Box<dyn Identity>>, String> {
    let mut identities: Vec<Box<dyn Identity>> = Vec::new();
    for path in files {
        let shown = path.display().to_string();
        let bytes = Zeroizing::new(std::fs::read(path).map_err(|e| format!("age identity file {shown}: {e}"))?);
        if bytes.starts_with(b"age-encryption.org/v1") || bytes.starts_with(b"-----BEGIN AGE ENCRYPTED FILE-----") {
            let prompt = Prompt {
                interactive,
                file: shown.clone(),
            };
            let reader = ArmoredReader::new(std::io::Cursor::new(bytes.to_vec()));
            let id = age::encrypted::Identity::from_buffer(reader, Some(shown.clone()), prompt, None)
                .map_err(|e| format!("age identity file {shown}: {e}"))?
                .ok_or_else(|| format!("age identity file {shown} is not a passphrase-protected identity"))?;
            identities.push(Box::new(id));
        } else {
            let file =
                age::IdentityFile::from_buffer(&bytes[..]).map_err(|e| format!("age identity file {shown}: {e}"))?;
            let loaded = file
                .into_identities()
                .map_err(|e| format!("age identity file {shown}: {e}"))?;
            identities.extend(loaded.into_iter().map(|i| i as Box<dyn Identity>));
        }
    }
    Ok(identities)
}

/// Terminal passphrase prompt, refused unless interaction is allowed.
#[derive(Clone)]
struct Prompt {
    interactive: bool,
    file: String,
}

impl Callbacks for Prompt {
    fn display_message(&self, _: &str) {}

    fn confirm(&self, _: &str, _: &str, _: Option<&str>) -> Option<bool> {
        None
    }

    fn request_public_string(&self, _: &str) -> Option<String> {
        None
    }

    fn request_passphrase(&self, _: &str) -> Option<SecretString> {
        if !self.interactive {
            return None;
        }
        rpassword::prompt_password(format!("Passphrase for age identity {}: ", self.file))
            .ok()
            .map(SecretString::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use age::secrecy::ExposeSecret;

    fn keypair() -> (Box<dyn Identity>, x25519::Recipient, String) {
        let id = x25519::Identity::generate();
        let text = id.to_string().expose_secret().to_owned();
        let public = id.to_public();
        (Box::new(id), public, text)
    }

    #[test]
    fn roundtrip_to_several_recipients() {
        let (a, ra, _) = keypair();
        let (b, rb, _) = keypair();
        let ct = encrypt(b"secret\n", &[ra, rb]).unwrap();
        assert!(ct.starts_with(b"age-encryption.org/v1"));
        assert_eq!(stanza_count(&ct), 2);
        assert_eq!(&*decrypt(&ct, &[a]).unwrap(), b"secret\n");
        assert_eq!(&*decrypt(&ct, &[b]).unwrap(), b"secret\n");
    }

    #[test]
    fn wrong_identity_is_distinct_from_corruption() {
        let (_, ra, _) = keypair();
        let (other, _, _) = keypair();
        let ct = encrypt(b"secret", &[ra]).unwrap();
        assert!(matches!(decrypt(&ct, &[other]), Err(Failure::NoKey(_))));
        let (a, ra, _) = keypair();
        let ct = encrypt(&vec![7u8; 200_000], &[ra]).unwrap();
        assert!(matches!(decrypt(&ct[..ct.len() - 10], &[a]), Err(Failure::Corrupt(_))));
        let (a, _, _) = keypair();
        assert!(matches!(decrypt(b"\x85\x02not age", &[a]), Err(Failure::Corrupt(_))));
    }

    #[test]
    fn tampered_payload_is_rejected() {
        let (a, ra, _) = keypair();
        let mut ct = encrypt(b"secret value", &[ra]).unwrap();
        let last = ct.len() - 1;
        ct[last] ^= 1;
        assert!(matches!(decrypt(&ct, &[a]), Err(Failure::Corrupt(_))));
    }

    #[test]
    fn recipient_list_validation() {
        assert!(parse_recipients(&[]).unwrap_err().contains("empty"));
        let e = parse_recipients(&["age1notvalid".into()]).unwrap_err();
        assert!(e.contains("recipients[0]"), "{e}");
        let (_, r, _) = keypair();
        assert_eq!(parse_recipients(&[r.to_string()]).unwrap().len(), 1);
    }

    #[test]
    fn identity_file_checks() {
        let dir = tempfile::tempdir().unwrap();
        let (_, _, text) = keypair();
        let path = dir.path().join("id.txt");
        std::fs::write(&path, &text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(check_identity_file(&path, &[]).unwrap_err().contains("chmod 600"));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(check_identity_file(&path, &[]).is_ok());
        // The repository root is canonical in real use; on macOS the temp dir is not (/var -> /private/var).
        assert!(
            check_identity_file(&path, &[&dir.path().canonicalize().unwrap()])
                .unwrap_err()
                .contains("inside")
        );
        assert!(check_identity_file(&dir.path().join("none"), &[]).is_err());
    }

    #[test]
    fn loads_plain_identity_file() {
        let dir = tempfile::tempdir().unwrap();
        let (_, r, text) = keypair();
        let path = dir.path().join("id.txt");
        std::fs::write(&path, format!("# created by test\n{text}\n")).unwrap();
        let ids = load_identities(&[&path], false).unwrap();
        let ct = encrypt(b"x", &[r]).unwrap();
        assert_eq!(&*decrypt(&ct, &ids).unwrap(), b"x");
    }

    #[test]
    fn passphrase_identity_needs_interaction() {
        let dir = tempfile::tempdir().unwrap();
        let (_, r, text) = keypair();
        let enc = age::Encryptor::with_user_passphrase(SecretString::from("pw".to_owned()));
        let mut wrapped = Vec::new();
        let mut w = enc.wrap_output(&mut wrapped).unwrap();
        w.write_all(text.as_bytes()).unwrap();
        w.finish().unwrap();
        let path = dir.path().join("id.age");
        std::fs::write(&path, wrapped).unwrap();
        let ids = load_identities(&[&path], false).unwrap();
        let ct = encrypt(b"x", &[r]).unwrap();
        assert!(matches!(decrypt(&ct, &ids), Err(Failure::Locked(m)) if m.contains("--interactive")));
    }
}
