//! A system file that the user cannot write deploys through
//! `sudo rustow __sudo-helper`. The tests use a fake sudo (`RUSTOW_SUDO`):
//! it makes the read-only target directory writable, runs the helper as
//! the same user and logs each call. Home files never use sudo.

mod common;

use common::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

struct Sudo {
    fx: Fixture,
    script: PathBuf,
    log: PathBuf,
    etc: PathBuf,
}

const FAKE: &str = "#!/bin/sh\n\
    echo \"$*\" >> \"$FAKE_SUDO_LOG\"\n\
    [ \"$1\" = \"--\" ] && shift\n\
    chmod 755 \"$FAKE_SUDO_UNLOCK\"\n\
    \"$@\"\n\
    code=$?\n\
    chmod 555 \"$FAKE_SUDO_UNLOCK\"\n\
    exit $code\n";

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

impl Sudo {
    /// A repository with a registered system copy and a system template;
    /// `<root>/etc` exists and the user cannot write it.
    fn new(script: &str) -> Sudo {
        let fx = Fixture::new();
        fx.write(
            "rustow.toml",
            "version = 1\n[vars]\nv = \"t\"\n[[files]]\nsource = \"rustowroot/etc/app.conf\"\n",
        );
        fx.write("rustowroot/etc/app.conf", "a\n");
        fx.write("rustowroot/etc/t.conf.tmpl", "{{ vars.v }}\n");
        let etc = fx.root.join("etc");
        fs::create_dir_all(&etc).unwrap();
        set_mode(&etc, 0o555);
        let script_path = write_file(&fx.base.join("fake-sudo"), script);
        set_mode(&script_path, 0o755);
        let log = fx.base.join("sudo.log");
        Sudo {
            fx,
            script: script_path,
            log,
            etc,
        }
    }

    fn run(&self, args: &[&str]) -> Out {
        let env = [
            ("RUSTOW_SUDO", self.script.to_str().unwrap()),
            ("FAKE_SUDO_LOG", self.log.to_str().unwrap()),
            ("FAKE_SUDO_UNLOCK", self.etc.to_str().unwrap()),
        ];
        self.fx.run_env(args, &env)
    }

    fn calls(&self) -> usize {
        fs::read_to_string(&self.log).map_or(0, |l| l.lines().count())
    }
}

impl Drop for Sudo {
    fn drop(&mut self) {
        set_mode(&self.etc, 0o755);
    }
}

#[test]
fn a_system_file_you_cannot_write_goes_through_sudo() {
    let s = Sudo::new(FAKE);
    let dry = s.run(&["-n", "apply"]);
    assert_eq!(dry.code, 0, "{dry:?}");
    assert_eq!(dry.stdout.matches("via sudo").count(), 2, "{dry:?}");
    assert_eq!(s.calls(), 0, "a dry run never calls sudo");

    let out = s.run(&["apply"]);
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(s.calls(), 1, "one sudo call per run");
    assert!(read(&s.log).contains("__sudo-helper"), "{}", read(&s.log));
    assert_eq!(read(&s.fx.root_path("etc/app.conf")), "a\n");
    assert_eq!(read(&s.fx.root_path("etc/t.conf")), "t\n");

    let again = s.run(&["apply"]);
    assert_eq!(again.code, 0, "{again:?}");
    assert_eq!(s.calls(), 1, "nothing to write, no sudo");
    let status = s.run(&["status"]);
    assert_eq!(status.code, 0, "{status:?}");
}

#[test]
fn undeploy_removes_system_files_and_their_new_directories_through_sudo() {
    let s = Sudo::new(FAKE);
    let config = read(&s.fx.repo.join("rustow.toml"));
    s.fx.write(
        "rustow.toml",
        &format!("{config}[[files]]\nsource = \"rustowroot/etc/sub/x.conf\"\n"),
    );
    s.fx.write("rustowroot/etc/sub/x.conf", "x\n");
    assert_eq!(s.run(&["apply"]).code, 0);
    assert!(s.fx.root_path("etc/sub/x.conf").exists());
    assert_eq!(s.calls(), 1);

    let dry = s.run(&["-n", "undeploy", "rustowroot"]);
    assert_eq!(dry.code, 0, "{dry:?}");
    assert_eq!(dry.stdout.matches("via sudo").count(), 3, "{dry:?}");
    assert_eq!(s.calls(), 1, "a dry run never calls sudo");

    let out = s.run(&["undeploy", "rustowroot"]);
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(s.calls(), 2, "one sudo call for all removals");
    for f in ["etc/app.conf", "etc/t.conf", "etc/sub"] {
        assert!(!s.fx.root_path(f).exists(), "{f} is gone");
    }
    assert!(s.fx.root_path("etc").exists(), "rustow did not create etc");
    assert_eq!(s.fx.state_json()["entries"], serde_json::json!({}));
    assert_eq!(s.fx.state_json()["created_dirs"], serde_json::json!([]));
}

#[test]
fn a_failing_sudo_fails_the_items_and_writes_nothing() {
    let s =
        Sudo::new("#!/bin/sh\necho \"$*\" >> \"$FAKE_SUDO_LOG\"\necho 'sudo: a password is required' >&2\nexit 1\n");
    let out = s.run(&["apply"]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stdout.contains("sudo"), "{out:?}");
    assert!(!s.fx.root_path("etc/app.conf").exists());
    assert_eq!(s.calls(), 1);
    assert_eq!(s.run(&["status"]).code, 1, "nothing recorded");
}

#[test]
fn home_files_never_use_sudo() {
    let s = Sudo::new(FAKE);
    s.fx.write(".config/ro/x", "x\n");
    let ro = s.fx.home_path(".config/ro");
    fs::create_dir_all(&ro).unwrap();
    set_mode(&ro, 0o555);
    let out = s.run(&["link", ".config/ro"]);
    set_mode(&ro, 0o755);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stdout.contains("elevated privileges"), "{out:?}");

    s.fx.write(".config/rw/y", "y\n");
    let rw = s.fx.home_path(".config/rw");
    fs::create_dir_all(&rw).unwrap();
    s.fx.ok(&["link", ".config/rw"]);
    set_mode(&rw, 0o555);
    let out = s.run(&["unlink", ".config/rw"]);
    set_mode(&rw, 0o755);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(is_symlink(&rw.join("y")));
    assert_eq!(s.calls(), 0, "home files never use sudo");
}

/// A sudo program named without a `/` is looked up only in absolute PATH
/// entries: an empty entry would run `./fake-sudo` from the current directory.
#[test]
fn sudo_from_the_current_directory_never_runs() {
    let mut s = Sudo::new(FAKE);
    s.fx.cwd = s.fx.base.clone();
    let env = [
        ("RUSTOW_SUDO", "fake-sudo"),
        ("PATH", ":/usr/bin:/bin"),
        ("FAKE_SUDO_LOG", s.log.to_str().unwrap()),
        ("FAKE_SUDO_UNLOCK", s.etc.to_str().unwrap()),
    ];
    let out = s.fx.run_env(&["apply"], &env);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stdout.contains("fake-sudo: not found in PATH"), "{out:?}");
    assert_eq!(s.calls(), 0, "{out:?}");
}
