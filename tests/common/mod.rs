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
    /// Point RUSTOW_REPO at the repository (default). Discovery tests turn it off.
    pub env_repo: bool,
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
            env_repo: true,
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

    /// The repository's state file as JSON; `null` when there is none.
    pub fn state_json(&self) -> serde_json::Value {
        let file = fs::read_dir(self.state.join("rustow"))
            .into_iter()
            .flatten()
            .map(|e| e.unwrap().path())
            .find(|p| p.extension().is_some_and(|x| x == "json"));
        file.map_or(serde_json::Value::Null, |f| serde_json::from_str(&read(&f)).unwrap())
    }

    /// Runs rustow with HOME, XDG_STATE_HOME and --root-target isolated.
    pub fn run(&self, args: &[&str]) -> Out {
        self.run_env(args, &[])
    }

    pub fn run_env(&self, args: &[&str], env: &[(&str, &str)]) -> Out {
        output(self.command(args, env))
    }

    /// Runs with a pseudo terminal on stdin that already holds `input`.
    pub fn run_tty(&self, args: &[&str], input: &str) -> Out {
        let pty = nix::pty::openpty(None, None).unwrap();
        nix::unistd::write(&pty.master, input.as_bytes()).unwrap();
        let mut cmd = self.command(args, &[]);
        cmd.stdin(fs::File::from(pty.slave));
        output(cmd) // pty.master stays open until rustow exits
    }

    fn command(&self, args: &[&str], env: &[(&str, &str)]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rustow"));
        cmd.current_dir(&self.cwd)
            .env_clear()
            // Coverage runs write profiles here; without it they land in the repository.
            .envs(std::env::vars_os().filter(|(k, _)| k == "LLVM_PROFILE_FILE"))
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("XDG_STATE_HOME", &self.state)
            .env(
                "RUSTOW_REPO",
                if self.env_repo {
                    self.repo.as_os_str()
                } else {
                    "".as_ref()
                },
            )
            .env("NO_COLOR", "1")
            // A test that forgets VISUAL fails fast instead of waiting in vi.
            .env("EDITOR", "false")
            // Never the real sudo: a test that escalates by accident fails fast.
            .env("RUSTOW_SUDO", "false")
            .arg("--root-target")
            .arg(&self.root)
            .args(args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd
    }

    /// Like `run`, but fails instead of hanging or exhausting memory: rustow
    /// gets 1 GiB of address space and is killed after `secs` seconds.
    pub fn run_within(&self, secs: u64, args: &[&str]) -> Out {
        use std::os::unix::process::CommandExt;
        use std::time::{Duration, Instant};
        let mut cmd = self.command(args, &[]);
        let limit = nix::sys::resource::Resource::RLIMIT_AS;
        // SAFETY: setrlimit is async-signal-safe; nothing else runs between fork and exec.
        unsafe {
            cmd.pre_exec(move || Ok(nix::sys::resource::setrlimit(limit, 1 << 30, 1 << 30)?));
        }
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().expect("run rustow");
        let end = Instant::now() + Duration::from_secs(secs);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() > end {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("rustow {args:?} still ran after {secs} s");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        to_out(child.wait_with_output().unwrap())
    }

    /// Like `run_env`, with the resource limit `res` of rustow set to `limit`.
    pub fn run_limit(&self, res: nix::sys::resource::Resource, limit: u64, args: &[&str], env: &[(&str, &str)]) -> Out {
        use std::os::unix::process::CommandExt;
        let mut cmd = self.command(args, env);
        // A file size limit would cut the coverage profile, and a cut profile
        // fails the whole coverage merge: such a run writes its profile into
        // the working directory, which the fixture deletes.
        cmd.env_remove("LLVM_PROFILE_FILE");
        // SAFETY: setrlimit is async-signal-safe; nothing else runs between fork and exec.
        unsafe {
            cmd.pre_exec(move || Ok(nix::sys::resource::setrlimit(res, limit, limit)?));
        }
        output(cmd)
    }

    /// Like `run`, with the umask of rustow set to `mask`.
    pub fn run_umask(&self, mask: nix::sys::stat::Mode, args: &[&str]) -> Out {
        use std::os::unix::process::CommandExt;
        let mut cmd = self.command(args, &[]);
        // SAFETY: umask is async-signal-safe; nothing else runs between fork and exec.
        unsafe {
            cmd.pre_exec(move || {
                nix::sys::stat::umask(mask);
                Ok(())
            });
        }
        output(cmd)
    }

    /// Runs and asserts the exit code.
    pub fn ok(&self, args: &[&str]) -> Out {
        let out = self.run(args);
        assert_eq!(out.code, 0, "{args:?}\n{out:?}");
        out
    }
}

fn output(mut cmd: Command) -> Out {
    to_out(cmd.output().expect("run rustow"))
}

fn to_out(out: std::process::Output) -> Out {
    Out {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
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

    /// A copy of the identity of `key`, protected by the passphrase "pw" (as `age -p` makes it).
    pub fn age_key_locked(&self, key: &AgeKey) -> AgeKey {
        use std::io::Write;
        let enc = age::Encryptor::with_user_passphrase(age::secrecy::SecretString::from("pw".to_owned()));
        let mut wrapped = Vec::new();
        let mut w = enc.wrap_output(&mut wrapped).unwrap();
        w.write_all(&fs::read(&key.identity).unwrap()).unwrap();
        w.finish().unwrap();
        let identity = key.identity.with_extension("locked");
        fs::write(&identity, wrapped).unwrap();
        fs::set_permissions(&identity, fs::Permissions::from_mode(0o600)).unwrap();
        AgeKey {
            identity,
            recipient: key.recipient.clone(),
        }
    }

    /// `rustow.toml` with age recipients, identity files and `extra` TOML.
    pub fn age_config(&self, recipients: &[&AgeKey], identities: &[&AgeKey], extra: &str) {
        let list = |items: Vec<String>| items.iter().map(|s| format!("\"{s}\"")).collect::<Vec<_>>().join(", ");
        let r = list(recipients.iter().map(|k| k.recipient.clone()).collect());
        let i = list(identities.iter().map(|k| k.identity.display().to_string()).collect());
        self.write(
            "rustow.toml",
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

    /// Rewrites the age file at `path` with ASCII armor (as `age -a` makes it).
    pub fn age_armor(&self, path: &Path) {
        use std::io::Write;
        let mut out = Vec::new();
        let mut w = age::armor::ArmoredWriter::wrap_output(&mut out, age::armor::Format::AsciiArmor).unwrap();
        w.write_all(&fs::read(path).unwrap()).unwrap();
        w.finish().unwrap();
        fs::write(path, out).unwrap();
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

/// True when gpg can run. Missing gpg skips locally but fails in CI.
pub fn require_gpg() -> bool {
    if Command::new("gpg")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        return true;
    }
    assert!(std::env::var_os("CI").is_none(), "GPG REQUIRED: gpg is missing in CI");
    eprintln!("SKIP: gpg is not installed; GPG tests did not run");
    false
}

/// A disposable GNUPGHOME with one generated key; the agent dies on drop.
pub struct GpgHome {
    pub dir: PathBuf,
    pub fpr: String,
}

impl Drop for GpgHome {
    fn drop(&mut self) {
        let _ = Command::new("gpgconf")
            .arg("--homedir")
            .arg(&self.dir)
            .args(["--kill", "all"])
            .output();
    }
}

impl GpgHome {
    pub fn gpg(&self) -> Command {
        let mut cmd = Command::new("gpg");
        cmd.arg("--homedir")
            .arg(&self.dir)
            .args(["--batch", "--no-tty", "--quiet"]);
        cmd
    }

    /// Imports another home's public key (without trusting it).
    pub fn import_public(&self, other: &GpgHome) {
        let exported = other.gpg().args(["--export", &other.fpr]).output().unwrap().stdout;
        let mut child = self
            .gpg()
            .arg("--import")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(child.stdin.as_mut().unwrap(), &exported).unwrap();
        assert!(child.wait().unwrap().success());
    }

    pub fn decrypt(&self, path: &Path) -> String {
        let out = self
            .gpg()
            .args(["--pinentry-mode", "loopback", "--passphrase", "pw", "--decrypt"])
            .arg(path)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }
}

impl Fixture {
    /// A new GNUPGHOME under the fixture with a key; protected by "pw" when `locked`.
    pub fn gpg_home(&self, name: &str, locked: bool) -> GpgHome {
        let dir = self.base.join(format!("g-{name}"));
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        let mut home = GpgHome {
            dir,
            fpr: String::new(),
        };
        let pass = if locked { "pw" } else { "" };
        let uid = format!("rustow test {name} <{name}@example.invalid>");
        let out = home
            .gpg()
            .args([
                "--pinentry-mode",
                "loopback",
                "--passphrase",
                pass,
                "--quick-gen-key",
                &uid,
                "default",
                "default",
                "never",
            ])
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let colons = home
            .gpg()
            .args(["--with-colons", "--list-keys"])
            .output()
            .unwrap()
            .stdout;
        let colons = String::from_utf8(colons).unwrap();
        home.fpr = colons
            .lines()
            .find(|l| l.starts_with("fpr:"))
            .unwrap()
            .split(':')
            .nth(9)
            .unwrap()
            .to_owned();
        home
    }

    /// `rustow.toml` with GPG recipients and home, plus `extra` TOML.
    pub fn gpg_config(&self, home: &GpgHome, fprs: &[&str], extra: &str) {
        let list = fprs.iter().map(|f| format!("\"{f}\"")).collect::<Vec<_>>().join(", ");
        self.write(
            "rustow.toml",
            &format!(
                "version = 1\n{extra}\n[encryption.gpg]\nrecipients = [{list}]\nhomedir = \"{}\"\n",
                home.dir.display()
            ),
        );
    }

    /// Writes OpenPGP ciphertext made by plain gpg to a repository path.
    pub fn gpg_encrypt(&self, rel: &str, plaintext: &str, home: &GpgHome) -> PathBuf {
        self.gpg_encrypt_to(rel, plaintext, home, &[&home.fpr])
    }

    /// Like `gpg_encrypt`, to each of `fprs` (public keys in `home`).
    pub fn gpg_encrypt_to(&self, rel: &str, plaintext: &str, home: &GpgHome, fprs: &[&str]) -> PathBuf {
        let path = self.repo.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut child = home
            .gpg()
            .args(["--trust-model", "always", "--encrypt"])
            .args(fprs.iter().flat_map(|f| ["--recipient", f]))
            .arg("--output")
            .arg(&path)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(child.stdin.as_mut().unwrap(), plaintext.as_bytes()).unwrap();
        assert!(child.wait().unwrap().success());
        path
    }
}

impl GpgHome {
    /// Generates another key in this home and returns its primary fingerprint.
    pub fn add_key(&self, name: &str) -> String {
        let uid = format!("rustow test {name} <{name}@example.invalid>");
        let out = self
            .gpg()
            .args([
                "--pinentry-mode",
                "loopback",
                "--passphrase",
                "",
                "--quick-gen-key",
                &uid,
                "default",
                "default",
                "never",
            ])
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let colons = String::from_utf8(
            self.gpg()
                .args(["--with-colons", "--list-keys"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let lines: Vec<&str> = colons.lines().collect();
        lines
            .windows(2)
            .filter(|w| w[0].starts_with("pub:") && w[1].starts_with("fpr:"))
            .map(|w| w[1].split(':').nth(9).unwrap().to_owned())
            .find(|f| *f != self.fpr)
            .unwrap()
    }

    /// Makes the agent use a stub pinentry that answers "pw". Each call
    /// appends a line to the returned log file.
    pub fn stub_pinentry(&self) -> PathBuf {
        self.stub_pinentry_answering("pw")
    }

    /// Like `stub_pinentry`, with `pin` as the answer.
    pub fn stub_pinentry_answering(&self, pin: &str) -> PathBuf {
        let log = self.dir.join("pinentry.log");
        let script = write_file(
            &self.dir.join("pinentry-stub"),
            &format!(
                "#!/bin/sh\necho called >> '{}'\necho 'OK stub'\nwhile read -r cmd rest; do\n  case \"$cmd\" in\n    GETPIN) echo 'D {pin}'; echo OK ;;\n    BYE) echo OK; exit 0 ;;\n    *) echo OK ;;\n  esac\ndone\n",
                log.display()
            ),
        );
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        write_file(
            &self.dir.join("gpg-agent.conf"),
            &format!("pinentry-program {}\n", script.display()),
        );
        let reload = Command::new("gpgconf")
            .arg("--homedir")
            .arg(&self.dir)
            .args(["--reload", "gpg-agent"])
            .output()
            .unwrap();
        assert!(reload.status.success(), "{}", String::from_utf8_lossy(&reload.stderr));
        log
    }

    /// Long key ids of the ENC_TO records of an OpenPGP file.
    pub fn enc_to(&self, path: &Path) -> Vec<String> {
        let out = self
            .gpg()
            .args(["--list-only", "--status-fd", "1", "--decrypt"])
            .arg(path)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.strip_prefix("[GNUPG:] ENC_TO "))
            .filter_map(|rest| rest.split(' ').next())
            .map(str::to_owned)
            .collect()
    }
}

/// Sets a POSIX access ACL on `path` that lets user `uid` read and write
/// it, in the kernel's xattr format (no `setfacl` needed).
#[cfg(target_os = "linux")]
pub fn grant_write(path: &Path, uid: u32) -> std::io::Result<()> {
    const ANY: u32 = u32::MAX;
    // (tag, perm, id): owner rw, named user rw, group r, mask rw, other r.
    let entries = [
        (0x01, 6, ANY),
        (0x02, 6, uid),
        (0x04, 4, ANY),
        (0x10, 6, ANY),
        (0x20, 4, ANY),
    ];
    let mut acl = 2u32.to_le_bytes().to_vec();
    for (tag, perm, id) in entries {
        acl.extend(u16::to_le_bytes(tag));
        acl.extend(u16::to_le_bytes(perm));
        acl.extend(u32::to_le_bytes(id));
    }
    let flags = rustix::fs::XattrFlags::empty();
    Ok(rustix::fs::setxattr(path, "system.posix_acl_access", &acl, flags)?)
}
