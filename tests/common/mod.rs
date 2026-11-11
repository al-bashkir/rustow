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
