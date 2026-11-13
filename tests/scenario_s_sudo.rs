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

/// The helper answers each job before it gets the next one. Here the
/// answer of the first job reaches rustow and the pipe breaks before the
/// second answer: the first file is recorded, the second item fails.
#[test]
fn each_helper_result_counts_as_it_arrives() {
    let s = Sudo::new(
        "#!/bin/sh\n[ \"$1\" = \"--\" ] && shift\nchmod 755 \"$FAKE_SUDO_UNLOCK\"\n\
         \"$@\" | { IFS= read -r l; exec 0<&-; printf '%s\\n' \"$l\"; }\n\
         chmod 555 \"$FAKE_SUDO_UNLOCK\"\nexit 1\n",
    );
    let out = s.run(&["apply"]);
    assert_eq!(out.code, 4, "{out:?}");
    let entries = &s.fx.state_json()["entries"];
    assert!(
        entries.get(s.fx.root_path("etc/app.conf").to_str().unwrap()).is_some(),
        "{entries}"
    );
    assert!(entries.get(s.fx.root_path("etc/t.conf").to_str().unwrap()).is_none());
    let failed = out
        .stdout
        .lines()
        .find(|l| l.contains("etc/t.conf"))
        .unwrap_or_default();
    assert!(failed.contains("fake-sudo failed (exit status: 1)"), "{out:?}");
}

/// rustow reads a copy source again when it writes it. Here the source of
/// the second system file changes while sudo asks for the password (same
/// length, other bytes): that item fails and nothing is written for it.
#[test]
fn a_source_changed_before_the_helper_writes_it_fails() {
    let s = Sudo::new(&FAKE.replacen('\n', "\nprintf 'Z\\n' > \"$FAKE_SOURCE\"\n", 1));
    let config = read(&s.fx.repo.join("rustow.toml"));
    s.fx.write(
        "rustow.toml",
        &format!("{config}[[files]]\nsource = \"rustowroot/etc/z.conf\"\n"),
    );
    let source = s.fx.write("rustowroot/etc/z.conf", "z\n");
    let env = [
        ("RUSTOW_SUDO", s.script.to_str().unwrap()),
        ("FAKE_SUDO_LOG", s.log.to_str().unwrap()),
        ("FAKE_SUDO_UNLOCK", s.etc.to_str().unwrap()),
        ("FAKE_SOURCE", source.to_str().unwrap()),
    ];
    let out = s.fx.run_env(&["apply"], &env);
    assert_eq!(out.code, 4, "{out:?}");
    let failed = out
        .stdout
        .lines()
        .find(|l| l.contains("etc/z.conf"))
        .unwrap_or_default();
    assert!(failed.contains("content changed while rustow was running"), "{out:?}");
    assert!(!s.fx.root_path("etc/z.conf").exists());
    assert_eq!(read(&s.fx.root_path("etc/app.conf")), "a\n");
    let temps: Vec<_> = fs::read_dir(&s.etc).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(temps.len(), 2, "app.conf and t.conf, no temporary file: {temps:?}");
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

/// The lock stops a second run only while its file is in place. A file
/// that disappears during the run (here while sudo asks for the password)
/// lets another run start, so rustow warns before it saves the state.
#[test]
fn a_lock_file_removed_during_the_run_is_reported() {
    let s = Sudo::new(&FAKE.replacen('\n', "\nrm -f \"$XDG_STATE_HOME\"/rustow/*.lock\n", 1));
    let out = s.run(&["apply"]);
    assert_eq!(out.code, 0, "{out:?}");
    assert!(
        out.stderr.contains("warning: the lock file") && out.stderr.contains("was removed or replaced during this run"),
        "{out:?}"
    );
    assert_eq!(read(&s.fx.root_path("etc/app.conf")), "a\n");
    let again = s.run(&["apply"]);
    assert_eq!(again.code, 0, "the state was saved: {again:?}");
    assert!(!again.stderr.contains("warning"), "no sudo, no removal: {again:?}");
}

/// While sudo asks for the password, `etc` is moved away and `how` puts
/// something new in its place. Returns the fixture, the run and `<base>/other`.
fn swap_before_the_helper(how: &str) -> (Sudo, Out, PathBuf) {
    let script = format!(
        "#!/bin/sh\n[ \"$1\" = \"--\" ] && shift\nmv \"$FAKE_SUDO_UNLOCK\" \"$FAKE_SUDO_UNLOCK.real\"\n{how}\nexec \"$@\"\n"
    );
    let s = Sudo::new(&script);
    let other = s.fx.base.join("other");
    fs::create_dir(&other).unwrap();
    let out = s.run(&["apply"]);
    (s, out, other)
}

/// The helper walks the planned directory again and refuses another one
/// in its place: it writes nothing there.
#[test]
fn the_helper_refuses_a_directory_swapped_after_the_plan() {
    let (s, out, _) = swap_before_the_helper("mkdir \"$FAKE_SUDO_UNLOCK\"");
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stdout.contains("etc changed while rustow was running"), "{out:?}");
    assert_eq!(fs::read_dir(&s.etc).unwrap().count(), 0, "nothing in the new etc");
    assert_eq!(fs::read_dir(s.etc.with_extension("real")).unwrap().count(), 0);
}

/// A symlink to a directory the user owns passes the trust rule, but it
/// is not the planned directory either.
#[test]
fn the_helper_refuses_a_symlink_swapped_in_after_the_plan() {
    let link = "ln -s \"$(dirname \"$FAKE_SUDO_UNLOCK\")/../other\" \"$FAKE_SUDO_UNLOCK\"";
    let (s, out, other) = swap_before_the_helper(link);
    assert!(is_symlink(&s.etc), "{out:?}");
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stdout.contains("etc changed while rustow was running"), "{out:?}");
    assert_eq!(
        fs::read_dir(&other).unwrap().count(),
        0,
        "nothing in {}",
        other.display()
    );
}

/// The real helper runs as root: a directory that it made in the state
/// directory would be root's, and each later backup of the user would fail.
/// So rustow makes the backup directories as the user before sudo starts,
/// and the helper only moves the old file into them. The fake sudo takes
/// the helper's write access to the state directory away.
#[test]
fn the_backup_directories_of_a_system_file_are_made_by_the_user() {
    let s = Sudo::new(&FAKE.replacen(
        "\"$@\"\n",
        "chmod 500 \"$XDG_STATE_HOME/rustow\"\n\"$@\"\ncode=$?\nchmod 700 \"$XDG_STATE_HOME/rustow\"\n",
        1,
    ));
    set_mode(&s.etc, 0o755);
    fs::write(s.etc.join("app.conf"), "old\n").unwrap();
    set_mode(&s.etc, 0o555);
    s.fx.write(".homeconf", "new\n");
    s.fx.write_home(".homeconf", "old home\n");

    let out = s.run(&["apply", "--backup", "rustowroot"]);
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(s.calls(), 1);
    assert_eq!(read(&s.fx.root_path("etc/app.conf")), "a\n");
    let backups = s.fx.state.join("rustow/backups");
    let saved: Vec<_> = walk(&backups)
        .into_iter()
        .filter(|p| p.ends_with("etc/app.conf"))
        .collect();
    assert_eq!(saved.len(), 1, "{out:?}");
    assert_eq!(read(&saved[0]), "old\n");
    assert_eq!(mode(saved[0].parent().unwrap()), 0o700, "private");

    let home = s.run(&["apply", "--backup", ".homeconf"]);
    assert_eq!(home.code, 0, "{home:?}");
    assert!(is_symlink(&s.fx.home_path(".homeconf")));
    assert_eq!(s.calls(), 1, "a home file never uses sudo");
}

/// Every path below `dir`.
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).into_iter().flatten() {
        let path = entry.unwrap().path();
        out.extend(walk(&path));
        out.push(path);
    }
    out
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
