//! GPG backend: the installed `gpg` executable, run without a shell and
//! with a fixed option set that repository configuration cannot extend.
//! Success needs both a zero exit status and the operation's success
//! records on the machine-readable status channel (`--status-file`).

use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use zeroize::Zeroizing;

/// A located `gpg` and the keyring it uses.
#[derive(Debug, Clone)]
pub struct Gpg {
    pub program: PathBuf,
    pub homedir: PathBuf,
    /// Allow gpg-agent to show pinentry; otherwise fail instead of prompting.
    pub interactive: bool,
}

/// What a gpg run produced.
pub struct Run {
    pub success: bool,
    pub stdout: Zeroizing<Vec<u8>>,
    pub status: Vec<String>,
    pub stderr: String,
}

/// Checks a configured fingerprint: 40 (v4) or 64 (v5/v6) hex digits.
/// Names, e-mail addresses and short key ids are ambiguous and refused.
pub fn parse_fingerprint(text: &str) -> Result<String, String> {
    let fpr: String = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_uppercase();
    if matches!(fpr.len(), 40 | 64) && fpr.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(fpr);
    }
    Err(format!(
        "'{text}' is not a full key fingerprint (40 or 64 hex digits); names, e-mail addresses and short key ids are refused"
    ))
}

/// Status keywords and arguments, e.g. ("DECRYPTION_INFO", ["2", "9", "0"]).
fn records(lines: &[String]) -> Vec<(&str, Vec<&str>)> {
    lines
        .iter()
        .filter_map(|l| l.strip_prefix("[GNUPG:] "))
        .map(|l| {
            let mut words = l.split_whitespace();
            (words.next().unwrap_or(""), words.collect())
        })
        .collect()
}

type Record<'a> = (&'a str, Vec<&'a str>);

fn has(recs: &[Record], keyword: &str) -> bool {
    recs.iter().any(|(w, _)| *w == keyword)
}

/// Decryption succeeded only with a zero exit status, public-key
/// encryption, integrity protection (MDC or AEAD), DECRYPTION_OKAY and
/// END_DECRYPTION, and no failure record.
pub fn check_decrypt_status(lines: &[String], exit_ok: bool) -> Result<(), String> {
    let recs = records(lines);
    check_decrypt_keys(&recs)?;
    check_integrity(&recs)?;
    if !(has(&recs, "BEGIN_DECRYPTION") && has(&recs, "DECRYPTION_OKAY") && has(&recs, "END_DECRYPTION")) || !exit_ok {
        return Err("decryption did not complete successfully".into());
    }
    Ok(())
}

/// Public-key data, a usable secret key, and no failure record.
fn check_decrypt_keys(recs: &[Record]) -> Result<(), String> {
    if has(recs, "NEED_PASSPHRASE_SYM") || !has(recs, "ENC_TO") {
        return Err("not public-key encrypted OpenPGP data (symmetric-only ciphertext is not supported)".into());
    }
    if has(recs, "NO_SECKEY") && !has(recs, "DECRYPTION_OKAY") {
        return Err("no usable secret key for any recipient in the keyring".into());
    }
    if has(recs, "DECRYPTION_FAILED") || has(recs, "FAILURE") || has(recs, "ERROR") {
        return Err("decryption failed".into());
    }
    Ok(())
}

/// The DECRYPTION_INFO record must show MDC or AEAD protection.
fn check_integrity(recs: &[Record]) -> Result<(), String> {
    let protected = recs.iter().find(|(w, _)| *w == "DECRYPTION_INFO").map(|(_, args)| {
        let mdc = args.first().is_some_and(|m| *m != "0");
        let aead = args.get(2).is_some_and(|a| *a != "0");
        mdc || aead
    });
    match protected {
        None => Err("integrity protection could not be confirmed".into()),
        Some(false) => Err("message is not integrity protected; refused".into()),
        Some(true) => Ok(()),
    }
}

/// Encryption succeeded only with a zero exit status, END_ENCRYPTION, and
/// no invalid-recipient or failure record.
pub fn check_encrypt_status(lines: &[String], exit_ok: bool) -> Result<(), String> {
    let recs = records(lines);
    check_recipients(&recs)?;
    if has(&recs, "FAILURE") || has(&recs, "ERROR") || !has(&recs, "END_ENCRYPTION") || !exit_ok {
        return Err("gpg encryption did not complete successfully".into());
    }
    Ok(())
}

/// Fails on the first INV_RECP record, naming the recipient and the reason.
fn check_recipients(recs: &[Record]) -> Result<(), String> {
    match recs.iter().find(|(w, _)| *w == "INV_RECP") {
        Some((_, args)) => Err(format!(
            "recipient {} rejected: {}",
            args.get(1).unwrap_or(&"?"),
            inv_recp_reason(args.first().copied().unwrap_or("0"))
        )),
        None => Ok(()),
    }
}

/// Text for an INV_RECP reason code.
fn inv_recp_reason(code: &str) -> &'static str {
    match code {
        "1" => "not found",
        "2" => "ambiguous",
        "4" => "key revoked",
        "5" => "key expired",
        "10" => "key not trusted (rustow never overrides GPG trust; certify or trust the key in your keyring)",
        "13" => "key disabled",
        _ => "no usable encryption key",
    }
}

impl Gpg {
    /// Options for every run; nothing here comes from the repository.
    pub fn base_args(&self, status_file: &Path) -> Vec<OsString> {
        let pinentry = if self.interactive { "ask" } else { "error" };
        let mut args: Vec<OsString> = vec!["--no-options".into(), "--homedir".into(), self.homedir.clone().into()];
        args.extend(["--batch", "--no-tty", "--quiet", "--status-file"].map(OsString::from));
        args.push(status_file.into());
        args.extend(
            [
                "--disable-dirmngr",
                "--no-auto-key-retrieve",
                "--auto-key-locate",
                "local",
                "--no-default-recipient",
                "--no-encrypt-to",
                "--pinentry-mode",
                pinentry,
            ]
            .map(OsString::from),
        );
        args
    }

    /// Runs gpg with `args`, feeding `input` on stdin. Output, diagnostics
    /// and the status file are drained without deadlock.
    fn run(&self, args: &[&str], input: &[u8]) -> Result<Run, String> {
        let dir = tempfile::Builder::new()
            .prefix("rustow-gpg-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .map_err(|e| format!("gpg status directory: {e}"))?;
        let status_file = dir.path().join("status");
        let mut child = Command::new(&self.program)
            .args(self.base_args(&status_file))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot run {}: {e}", self.program.display()))?;
        let (Some(mut stdin), Some(mut stdout), Some(mut stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err("gpg pipes are unavailable".into());
        };
        let input = Zeroizing::new(input.to_vec());
        let writer = std::thread::spawn(move || {
            // A write error means gpg stopped reading; its status says why.
            let _ = stdin.write_all(&input);
        });
        let diagnostics = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            text
        });
        let mut out = Zeroizing::new(Vec::new());
        let read = stdout.read_to_end(&mut out);
        let status = child.wait().map_err(|e| format!("wait for gpg: {e}"))?;
        let _ = writer.join();
        let stderr = diagnostics.join().unwrap_or_default();
        read.map_err(|e| format!("read gpg output: {e}"))?;
        let status_lines = std::fs::read_to_string(&status_file)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect();
        Ok(Run {
            success: status.success(),
            stdout: out,
            status: status_lines,
            stderr,
        })
    }

    /// Decrypts and fully verifies before returning plaintext.
    pub fn decrypt(&self, ciphertext: &[u8]) -> Result<Zeroizing<Vec<u8>>, String> {
        let run = self.run(&["--decrypt"], ciphertext)?;
        check_decrypt_status(&run.status, run.success).map_err(|e| self.explain(e, &run))?;
        Ok(run.stdout)
    }

    /// Binary OpenPGP to every fingerprint; GPG trust rules apply.
    pub fn encrypt(&self, plaintext: &[u8], fingerprints: &[String]) -> Result<Vec<u8>, String> {
        let mut args = vec!["--encrypt"];
        for f in fingerprints {
            args.extend(["--recipient", f.as_str()]);
        }
        let run = self.run(&args, plaintext)?;
        check_encrypt_status(&run.status, run.success).map_err(|e| self.explain(e, &run))?;
        Ok(run.stdout.to_vec())
    }

    /// Confirms a fingerprint names exactly one valid primary key in the
    /// local keyring with encryption capability. GPG then picks the newest
    /// valid encryption subkey of that key.
    pub fn resolve(&self, fingerprint: &str) -> Result<(), String> {
        let run = self.run(&["--with-colons", "--list-keys", "--", fingerprint], b"")?;
        let text = String::from_utf8_lossy(&run.stdout);
        let keys = primary_keys(&text);
        let found: Vec<_> = keys.iter().filter(|k| k.2 == fingerprint).collect();
        let [(validity, caps, _)] = found.as_slice() else {
            let where_ = self.homedir.display();
            return Err(format!(
                "key {fingerprint} is not in the keyring {where_} (rustow never fetches keys)"
            ));
        };
        match validity.as_str() {
            "r" => return Err(format!("key {fingerprint} is revoked")),
            "e" => return Err(format!("key {fingerprint} is expired")),
            "i" | "n" => return Err(format!("key {fingerprint} is invalid")),
            _ => {}
        }
        if !caps.contains('E') || caps.contains('D') {
            return Err(format!("key {fingerprint} has no usable encryption subkey"));
        }
        Ok(())
    }

    /// Number of recipients (ENC_TO records) without decrypting.
    pub fn recipient_count(&self, ciphertext: &[u8]) -> Option<usize> {
        let run = self.run(&["--list-only", "--decrypt"], ciphertext).ok()?;
        Some(records(&run.status).iter().filter(|(w, _)| *w == "ENC_TO").count())
    }

    fn explain(&self, error: String, run: &Run) -> String {
        let key_problem = run
            .status
            .iter()
            .any(|l| l.contains("NO_SECKEY") || l.contains("pkdecrypt_failed"));
        let hint = if key_problem && !self.interactive {
            " (if the key is passphrase-protected, unlock it in gpg-agent or rerun with --interactive)"
        } else {
            ""
        };
        let detail = run
            .stderr
            .lines()
            .find(|l| !l.trim().is_empty())
            .map(|l| format!("; gpg: {}", l.trim()))
            .unwrap_or_default();
        format!("{error}{hint}{detail}")
    }
}

/// (validity, capabilities, fingerprint) of each primary key in `--with-colons` output.
fn primary_keys(colons: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut pending: Option<(String, String)> = None;
    for line in colons.lines() {
        let f: Vec<&str> = line.split(':').collect();
        match f.first().copied() {
            Some("pub") => {
                pending = Some((
                    f.get(1).unwrap_or(&"").to_string(),
                    f.get(11).unwrap_or(&"").to_string(),
                ))
            }
            Some("fpr") => {
                if let Some((validity, caps)) = pending.take() {
                    out.push((validity, caps, f.get(9).unwrap_or(&"").to_ascii_uppercase()));
                }
            }
            _ => {}
        }
    }
    out
}

/// A GPG home must be an existing directory of the current user that
/// others cannot write, outside the repository and cache.
pub fn check_homedir(home: &Path, forbidden: &[&Path]) -> Result<(), String> {
    let shown = home.display();
    let meta = std::fs::metadata(home).map_err(|e| format!("GPG home {shown}: {e}"))?;
    let real = home.canonicalize().map_err(|e| format!("GPG home {shown}: {e}"))?;
    if forbidden.iter().any(|d| real.starts_with(d)) {
        return Err(format!(
            "GPG home {shown} is inside the repository or cache; keep keyrings outside"
        ));
    }
    if !meta.is_dir() || meta.uid() != nix::unistd::geteuid().as_raw() || meta.permissions().mode() & 0o022 != 0 {
        return Err(format!(
            "GPG home {shown} must be your own directory, not writable by others (chmod 700)"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_owned).collect()
    }

    const GOOD: &str = "[GNUPG:] ENC_TO D6566C2BE3C65A95 18 0\n[GNUPG:] DECRYPTION_KEY 85 70 u\n\
        [GNUPG:] BEGIN_DECRYPTION\n[GNUPG:] DECRYPTION_INFO 2 9 0\n[GNUPG:] PLAINTEXT 62 0\n\
        [GNUPG:] DECRYPTION_OKAY\n[GNUPG:] GOODMDC\n[GNUPG:] END_DECRYPTION";

    #[test]
    fn good_decrypt_status_passes() {
        assert!(check_decrypt_status(&lines(GOOD), true).is_ok());
        let aead = GOOD.replace("DECRYPTION_INFO 2 9 0", "DECRYPTION_INFO 0 9 2");
        assert!(check_decrypt_status(&lines(&aead), true).is_ok());
    }

    #[test]
    fn decrypt_status_needs_exit_status_too() {
        assert!(check_decrypt_status(&lines(GOOD), false).is_err());
    }

    #[test]
    fn decrypt_status_rejects_missing_okay_or_end() {
        assert!(check_decrypt_status(&lines(&GOOD.replace("[GNUPG:] DECRYPTION_OKAY\n", "")), true).is_err());
        assert!(check_decrypt_status(&lines(&GOOD.replace("\n[GNUPG:] END_DECRYPTION", "")), true).is_err());
    }

    #[test]
    fn decrypt_status_rejects_missing_integrity_protection() {
        let e = check_decrypt_status(
            &lines(&GOOD.replace("DECRYPTION_INFO 2 9 0", "DECRYPTION_INFO 0 9 0")),
            true,
        );
        assert!(e.unwrap_err().contains("integrity"));
        let none = GOOD.replace("[GNUPG:] DECRYPTION_INFO 2 9 0\n", "");
        assert!(
            check_decrypt_status(&lines(&none), true)
                .unwrap_err()
                .contains("integrity")
        );
    }

    #[test]
    fn decrypt_status_rejects_symmetric_and_failures() {
        let sym = "[GNUPG:] NEED_PASSPHRASE_SYM 9 3 10\n[GNUPG:] BEGIN_DECRYPTION\n[GNUPG:] DECRYPTION_INFO 2 9 0\n\
            [GNUPG:] DECRYPTION_OKAY\n[GNUPG:] END_DECRYPTION";
        assert!(
            check_decrypt_status(&lines(sym), true)
                .unwrap_err()
                .contains("public-key")
        );
        let failed = format!("{GOOD}\n[GNUPG:] DECRYPTION_FAILED");
        assert!(check_decrypt_status(&lines(&failed), true).is_err());
        let nokey = "[GNUPG:] ENC_TO AAAA 18 0\n[GNUPG:] NO_SECKEY AAAA\n[GNUPG:] BEGIN_DECRYPTION\n\
            [GNUPG:] DECRYPTION_FAILED\n[GNUPG:] END_DECRYPTION";
        assert!(
            check_decrypt_status(&lines(nokey), false)
                .unwrap_err()
                .contains("secret key")
        );
    }

    #[test]
    fn misleading_status_lines_are_ignored() {
        // Only lines with the exact "[GNUPG:] " prefix count.
        let fake = "DECRYPTION_OKAY\n[GNUPG:]DECRYPTION_OKAY\n x [GNUPG:] END_DECRYPTION";
        assert!(check_decrypt_status(&lines(fake), true).is_err());
    }

    #[test]
    fn encrypt_status_cases() {
        let ok = "[GNUPG:] KEY_CONSIDERED F 0\n[GNUPG:] BEGIN_ENCRYPTION 2 9\n[GNUPG:] END_ENCRYPTION";
        assert!(check_encrypt_status(&lines(ok), true).is_ok());
        assert!(check_encrypt_status(&lines(ok), false).is_err());
        let untrusted = "[GNUPG:] INV_RECP 10 ABCDEF\n[GNUPG:] FAILURE encrypt 53";
        assert!(
            check_encrypt_status(&lines(untrusted), false)
                .unwrap_err()
                .contains("not trusted")
        );
        assert!(check_encrypt_status(&lines("[GNUPG:] BEGIN_ENCRYPTION 2 9"), true).is_err());
    }

    #[test]
    fn fingerprint_rules() {
        let v4 = "70724AA6092233B04A3002C3F3E77E27DD87359D";
        assert_eq!(parse_fingerprint(v4).unwrap(), v4);
        assert_eq!(
            parse_fingerprint("7072 4aa6 0922 33b0 4a30  02c3 f3e7 7e27 dd87 359d").unwrap(),
            v4
        );
        assert_eq!(parse_fingerprint(&"a".repeat(64)).unwrap().len(), 64);
        for bad in [
            "DD87359D",
            "F3E77E27DD87359D",
            "user@example.com",
            "0x70724AA6092233B04A3002C3F3E77E27DD87359D!",
            "",
        ] {
            assert!(parse_fingerprint(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn argv_is_fixed_and_safe() {
        let gpg = Gpg {
            program: "/usr/bin/gpg".into(),
            homedir: "/h/.gnupg".into(),
            interactive: false,
        };
        let args: Vec<String> = gpg
            .base_args(Path::new("/t/status"))
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let joined = args.join(" ");
        for must in [
            "--no-options",
            "--batch",
            "--no-tty",
            "--disable-dirmngr",
            "--no-auto-key-retrieve",
            "--no-encrypt-to",
        ] {
            assert!(args.iter().any(|a| a == must), "{must}: {joined}");
        }
        assert!(joined.contains("--homedir /h/.gnupg"), "{joined}");
        assert!(joined.contains("--status-file /t/status"), "{joined}");
        assert!(joined.contains("--pinentry-mode error"), "{joined}");
        let interactive = Gpg {
            interactive: true,
            ..gpg
        };
        let joined = interactive
            .base_args(Path::new("/t/s"))
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(joined.contains("--pinentry-mode ask"), "{joined}");
    }
}
