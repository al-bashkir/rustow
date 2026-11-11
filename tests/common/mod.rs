//! Shared fixture for integration tests: a temporary HOME holding a
//! dotfiles repository, a separate root target and a private state dir.
//! Nothing outside the temporary directory is touched.
#![allow(dead_code)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Fixture {
    _tmp: tempfile::TempDir,
    pub base: PathBuf,
    pub home: PathBuf,
    pub repo: PathBuf,
    pub root: PathBuf,
    pub state: PathBuf,
    /// Working directory for `run`; the repository by default.
    pub cwd: PathBuf,
}

pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl std::fmt::Debug for Out {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "exit {}\n--- stdout\n{}--- stderr\n{}",
            self.code, self.stdout, self.stderr
        )
    }
}

impl Fixture {
    pub fn new() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let home = base.join("home");
        let repo = home.join("dotfiles");
        let root = base.join("root");
        let state = base.join("state");
        for d in [&repo, &root] {
            fs::create_dir_all(d).unwrap();
        }
        Fixture {
            _tmp: tmp,
            base,
            home,
            cwd: repo.clone(),
            repo,
            root,
            state,
        }
    }

    /// Writes a repository file, creating parents.
    pub fn write(&self, rel: &str, content: &str) -> PathBuf {
        write_file(&self.repo.join(rel), content)
    }

    /// Writes a file under HOME, creating parents.
    pub fn write_home(&self, rel: &str, content: &str) -> PathBuf {
        write_file(&self.home.join(rel), content)
    }

    pub fn home_path(&self, rel: &str) -> PathBuf {
        self.home.join(rel)
    }

    pub fn root_path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// Runs rustow with HOME, XDG_STATE_HOME and --root-target isolated.
    pub fn run(&self, args: &[&str]) -> Out {
        self.run_env(args, &[])
    }

    pub fn run_env(&self, args: &[&str], env: &[(&str, &str)]) -> Out {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rustow"));
        cmd.current_dir(&self.cwd)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("XDG_STATE_HOME", &self.state)
            .env("NO_COLOR", "1")
            // A test that forgets VISUAL fails fast instead of waiting in vi.
            .env("EDITOR", "false")
            .arg("--root-target")
            .arg(&self.root)
            .args(args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("run rustow");
        Out {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    /// Runs and asserts the exit code.
    pub fn ok(&self, args: &[&str]) -> Out {
        let out = self.run(args);
        assert_eq!(out.code, 0, "{args:?}\n{out:?}");
        out
    }
}

pub fn write_file(path: &Path, content: &str) -> PathBuf {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
    path.to_path_buf()
}

pub fn link_text(path: &Path) -> PathBuf {
    fs::read_link(path).unwrap_or_else(|e| panic!("{} is not a symlink: {e}", path.display()))
}

pub fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

pub fn is_regular(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_file())
}

pub fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

pub fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// lstat modification time, to prove that nothing was rewritten.
pub fn mtime(path: &Path) -> std::time::SystemTime {
    fs::symlink_metadata(path).unwrap().modified().unwrap()
}

/// A disposable age key pair; the identity file lives outside the repository.
pub struct AgeKey {
    pub identity: PathBuf,
    pub recipient: String,
}

impl Fixture {
    pub fn age_key(&self, name: &str) -> AgeKey {
        use age::secrecy::ExposeSecret;
        let id = age::x25519::Identity::generate();
        let identity = write_file(
            &self.base.join("keys").join(format!("{name}.txt")),
            id.to_string().expose_secret(),
        );
        fs::set_permissions(&identity, fs::Permissions::from_mode(0o600)).unwrap();
        AgeKey {
            identity,
            recipient: id.to_public().to_string(),
        }
    }

    /// `.rustow.toml` with age recipients, identity files and `extra` TOML.
    pub fn age_config(&self, recipients: &[&AgeKey], identities: &[&AgeKey], extra: &str) {
        let list = |items: Vec<String>| items.iter().map(|s| format!("\"{s}\"")).collect::<Vec<_>>().join(", ");
        let r = list(recipients.iter().map(|k| k.recipient.clone()).collect());
        let i = list(identities.iter().map(|k| k.identity.display().to_string()).collect());
        self.write(
            ".rustow.toml",
            &format!("version = 1\n{extra}\n[encryption]\nrecipients = [{r}]\nidentity_files = [{i}]\n"),
        );
    }

    /// Writes age ciphertext of `plaintext` to a repository path.
    pub fn age_encrypt(&self, rel: &str, plaintext: &str, to: &[&AgeKey]) -> PathBuf {
        use std::io::Write;
        use std::str::FromStr;
        let recipients: Vec<age::x25519::Recipient> = to
            .iter()
            .map(|k| age::x25519::Recipient::from_str(&k.recipient).unwrap())
            .collect();
        let enc = age::Encryptor::with_recipients(recipients.iter().map(|r| r as &dyn age::Recipient)).unwrap();
        let mut out = Vec::new();
        let mut w = enc.wrap_output(&mut out).unwrap();
        w.write_all(plaintext.as_bytes()).unwrap();
        w.finish().unwrap();
        let path = self.repo.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, out).unwrap();
        path
    }

    /// Decrypts an age file with one key.
    pub fn age_decrypt(&self, path: &Path, key: &AgeKey) -> String {
        use std::io::Read;
        let ids = age::IdentityFile::from_file(key.identity.display().to_string())
            .unwrap()
            .into_identities()
            .unwrap();
        let bytes = fs::read(path).unwrap();
        let d = age::Decryptor::new(&bytes[..]).unwrap();
        let mut r = d.decrypt(ids.iter().map(|i| i.as_ref() as &dyn age::Identity)).unwrap();
        let mut s = String::new();
        r.read_to_string(&mut s).unwrap();
        s
    }

    /// An executable editor script; `$1` is the file to edit.
    pub fn editor(&self, name: &str, body: &str) -> String {
        let path = write_file(&self.base.join("bin").join(name), &format!("#!/bin/sh\n{body}\n"));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    }

    /// Edit workspaces left in the cache.
    pub fn workspaces(&self) -> Vec<PathBuf> {
        fs::read_dir(self.repo.join(".rustowcache/edit"))
            .map(|d| d.flatten().map(|e| e.path()).collect())
            .unwrap_or_default()
    }
}
