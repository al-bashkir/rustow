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
        let _ = fs::set_permissions(self.etc.join("hidden"), fs::Permissions::from_mode(0o755));
    }
}

/// A fake sudo that also lets the helper search `etc/hidden`, as root can.
const SEARCHES: &str = "#!/bin/sh\n\
    echo \"$*\" >> \"$FAKE_SUDO_LOG\"\n\
    [ \"$1\" = \"--\" ] && shift\n\
    chmod 755 \"$FAKE_SUDO_UNLOCK\" \"$FAKE_SUDO_UNLOCK/hidden\"\n\
    \"$@\"\n\
    code=$?\n\
    chmod 000 \"$FAKE_SUDO_UNLOCK/hidden\"\n\
    chmod 555 \"$FAKE_SUDO_UNLOCK\"\n\
    exit $code\n";

/// `etc/hidden`, a directory that the user cannot search (mode 0000), and
/// a `[[files]]` entry for `etc/hidden/x.conf` with `mode`.
fn hidden(s: &Sudo, mode: &str) -> PathBuf {
    let config = read(&s.fx.repo.join("rustow.toml"));
    s.fx.write(
        "rustow.toml",
        &format!("{config}[[files]]\nsource = \"rustowroot/etc/hidden/x.conf\"\nmode = \"{mode}\"\n"),
    );
    s.fx.write("rustowroot/etc/hidden/x.conf", "x\n");
    let dir = s.etc.join("hidden");
    set_mode(&s.etc, 0o755);
    fs::create_dir(&dir).unwrap();
    set_mode(&dir, 0o000);
    set_mode(&s.etc, 0o555);
    dir.join("x.conf")
}

/// Runs `f` while the user can search `etc/hidden`.
fn uncovered<T>(s: &Sudo, f: impl FnOnce() -> T) -> T {
    set_mode(&s.etc, 0o755);
    set_mode(&s.etc.join("hidden"), 0o755);
    let out = f();
    set_mode(&s.etc.join("hidden"), 0o000);
    set_mode(&s.etc, 0o555);
    out
}

/// The status line of `rel` in `out`.
fn line_of<'a>(out: &'a Out, rel: &str) -> &'a str {
    out.stdout.lines().find(|l| l.contains(rel)).unwrap_or_default()
}

/// A system file in a directory that the user cannot search (as
/// `/etc/sudoers.d`, mode 0750, owner root): rustow cannot see it, so it
/// compares it with the state. Without a record it creates it through
/// sudo; with one, it is clean while the desired content and metadata
/// are the recorded ones, and an update through sudo when they change.
#[test]
fn a_system_file_below_a_directory_you_cannot_search_is_compared_with_the_state() {
    let s = Sudo::new(SEARCHES);
    let x = hidden(&s, "0640");
    let dry = s.run(&["-n", "apply"]);
    assert_eq!(dry.code, 0, "{dry:?}");
    let line = line_of(&dry, "hidden/x.conf");
    assert!(line.contains("create") && line.contains("via sudo"), "{dry:?}");
    let status = s.run(&["status"]);
    assert_eq!(status.code, 1, "{status:?}");
    assert!(line_of(&status, "hidden/x.conf").contains("MISSING"), "{status:?}");
    assert_eq!(s.calls(), 0, "a dry run and status never call sudo");

    let out = s.run(&["apply"]);
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(s.calls(), 1, "{out:?}");
    assert_eq!(uncovered(&s, || (read(&x), mode(&x))), ("x\n".into(), 0o640));

    let again = s.run(&["apply"]);
    assert_eq!(again.code, 0, "{again:?}");
    assert_eq!(s.calls(), 1, "nothing to change, no sudo");
    let status = s.run(&["status", "-v"]);
    assert_eq!(status.code, 0, "{status:?}");
    let line = line_of(&status, "hidden/x.conf");
    assert!(
        line.contains("COPIED") && line.contains("not readable by you; compared with the state"),
        "{status:?}"
    );
    let diff = s.run(&["diff"]);
    assert_eq!((diff.code, diff.stdout.as_str()), (0, ""), "{diff:?}");

    // Only the mode changes: the state records the metadata too.
    let config = read(&s.fx.repo.join("rustow.toml"));
    s.fx.write("rustow.toml", &config.replace("0640", "0600"));
    let dry = s.run(&["-n", "apply"]);
    let line = line_of(&dry, "hidden/x.conf");
    assert!(line.contains("update") && line.contains("via sudo"), "{dry:?}");
    let diff = s.run(&["diff"]);
    assert_eq!(diff.code, 4, "{diff:?}");
    assert!(diff.stderr.contains("cannot read the destination"), "{diff:?}");
    let out = s.run(&["apply"]);
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(s.calls(), 2, "{out:?}");
    assert_eq!(uncovered(&s, || mode(&x)), 0o600);

    let gone = s.run(&["undeploy", "rustowroot/etc/hidden"]);
    assert_eq!(gone.code, 0, "{gone:?}");
    assert!(line_of(&gone, "hidden/x.conf").contains("via sudo"), "{gone:?}");
    assert!(!uncovered(&s, || x.exists()), "{gone:?}");
}

/// A client that is killed after it sent a job records nothing, but the
/// helper can still write the file. The next run finds the desired content
/// where the state names the old one: it writes it again and records it.
#[test]
fn a_file_that_the_helper_wrote_without_a_record_is_not_a_conflict() {
    let s = Sudo::new(SEARCHES);
    let x = hidden(&s, "0640");
    let state_file = || {
        let dir = s.fx.state.join("rustow");
        let found = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path());
        found
            .into_iter()
            .find(|p| p.extension().is_some_and(|e| e == "json"))
            .unwrap()
    };
    assert_eq!(s.run(&["apply"]).code, 0);
    let before = fs::read(state_file()).unwrap();
    s.fx.write("rustowroot/etc/hidden/x.conf", "y\n");
    assert_eq!(s.run(&["apply"]).code, 0);
    // The state names the old content.
    fs::write(state_file(), &before).unwrap();
    let out = s.run(&["apply"]);
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(uncovered(&s, || read(&x)), "y\n");

    // The state names no file: the first create was not recorded.
    fs::remove_file(state_file()).unwrap();
    let out = s.run(&["apply"]);
    assert_eq!(out.code, 0, "{out:?}");
    let calls = s.calls();
    assert_eq!(s.run(&["apply"]).code, 0);
    assert_eq!(s.calls(), calls, "recorded: no sudo");
}

/// The helper looks, as root, at what the plan could not see. A file that
/// rustow did not write (no record), or another content than the recorded
/// one, is a conflict, and the file stays; `--force` replaces it.
#[test]
fn the_helper_finds_a_conflict_below_a_directory_you_cannot_search() {
    let s = Sudo::new(SEARCHES);
    let x = hidden(&s, "0644");
    uncovered(&s, || fs::write(&x, "theirs\n").unwrap());
    let out = s.run(&["apply", "rustowroot/etc/hidden"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(
        line_of(&out, "hidden/x.conf").contains("existing destination is not managed by rustow"),
        "{out:?}"
    );
    assert_eq!(uncovered(&s, || read(&x)), "theirs\n");
    let forced = s.run(&["apply", "--force", "rustowroot/etc/hidden"]);
    assert_eq!(forced.code, 0, "{forced:?}");
    assert_eq!(uncovered(&s, || read(&x)), "x\n");

    uncovered(&s, || fs::write(&x, "edited\n").unwrap());
    s.fx.write("rustowroot/etc/hidden/x.conf", "y\n");
    let out = s.run(&["apply", "rustowroot/etc/hidden"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(
        line_of(&out, "hidden/x.conf").contains("destination was modified since rustow last applied it"),
        "{out:?}"
    );
    assert_eq!(uncovered(&s, || read(&x)), "edited\n");

    // A symlink is no copy of rustow, even to the desired content.
    let target = s.fx.base.join("desired");
    fs::write(&target, "y\n").unwrap();
    uncovered(&s, || {
        fs::remove_file(&x).unwrap();
        std::os::unix::fs::symlink(&target, &x).unwrap();
    });
    let out = s.run(&["apply", "rustowroot/etc/hidden"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(uncovered(&s, || x.is_symlink()), "{out:?}");
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

/// The helper answers each job before it gets the next one. Here its
/// ready line and the answer of the first job reach rustow and the pipe
/// breaks before the second answer: the first file is recorded, the
/// second item fails.
#[test]
fn each_helper_result_counts_as_it_arrives() {
    let s = Sudo::new(
        "#!/bin/sh\n[ \"$1\" = \"--\" ] && shift\nchmod 755 \"$FAKE_SUDO_UNLOCK\"\n\
         \"$@\" | { IFS= read -r r; printf '%s\\n' \"$r\"; IFS= read -r l; exec 0<&-; printf '%s\\n' \"$l\"; }\n\
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

/// The temporary-file journal of a run that ended normally is gone, and
/// the next run gives no notice about a leftover.
fn no_leftover_notice(s: &Sudo) {
    let pending = fs::read_dir(s.fx.state.join("rustow"))
        .unwrap()
        .filter(|e| e.as_ref().unwrap().path().extension().is_some_and(|x| x == "pending"))
        .count();
    let doctor = s.fx.run(&["doctor"]);
    assert_eq!(pending, 0, "{doctor:?}");
    assert!(doctor.stdout.contains("ok  temp files: none"), "{doctor:?}");
}

/// The helper refuses the jobs: the planned directory became a symlink
/// to a directory that the user cannot search. The helper never made a
/// temporary file there, so rustow drops their journal entries; it does
/// not tell the user to remove them with sudo.
#[test]
fn a_refused_job_leaves_no_journal_entry() {
    let link = "chmod 000 \"$(dirname \"$FAKE_SUDO_UNLOCK\")/../other\"\n\
                ln -s \"$(dirname \"$FAKE_SUDO_UNLOCK\")/../other\" \"$FAKE_SUDO_UNLOCK\"";
    let (s, out, _) = swap_before_the_helper(link);
    assert!(out.stdout.contains("etc changed while rustow was running"), "{out:?}");
    no_leftover_notice(&s);
}

/// A job that the helper wrote, and jobs that never reached it (here the
/// sudo program is missing), leave no journal entry either, also in a
/// directory that the user cannot search.
#[test]
fn jobs_that_ended_or_never_started_leave_no_journal_entry() {
    let s = Sudo::new(SEARCHES);
    hidden(&s, "0644");
    assert_eq!(s.run(&["apply"]).code, 0);
    no_leftover_notice(&s);

    s.fx.write("rustowroot/etc/hidden/x.conf", "y\n");
    let env = [("RUSTOW_SUDO", "rustow-test-no-such-sudo")];
    let out = s.fx.run_env(&["apply"], &env);
    assert!(out.stdout.contains("not found in PATH"), "{out:?}");
    no_leftover_notice(&s);
}

/// The line that the helper prints when it starts.
const READY: &str = "rustow sudo helper ready";

/// sudo stops before the helper starts (a wrong password, say): no
/// helper ever said that it runs, so it made nothing, and rustow keeps
/// no journal entry, also in a directory that the user cannot search.
#[test]
fn a_sudo_that_never_starts_the_helper_leaves_no_journal_entry() {
    let s = Sudo::new("#!/bin/sh\necho 'sudo: 3 incorrect password attempts' >&2\nexit 1\n");
    hidden(&s, "0644");
    let out = s.run(&["apply", "rustowroot/etc/hidden"]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(
        line_of(&out, "hidden/x.conf").contains("fake-sudo failed (exit status: 1)"),
        "{out:?}"
    );
    no_leftover_notice(&s);
}

/// A job that rustow sent to a running helper and that got no answer
/// keeps its journal entry: rustow cannot tell if the helper started it.
/// First the helper stops at once, and the frame (larger than a pipe
/// buffer) does not fit into the pipe; then it reads the frame and
/// answers no result.
#[test]
fn a_job_without_an_answer_keeps_its_journal_entry() {
    let big = "x".repeat(1 << 20);
    let stops = format!("#!/bin/sh\necho '{READY}'\nexit 1\n");
    let no_result =
        format!("#!/bin/sh\necho '{READY}'\nIFS= read -r header\necho 'not a result'\nexec cat >/dev/null\n");
    for (fake, content) in [(stops.as_str(), big.as_str()), (no_result.as_str(), "y\n")] {
        let s = Sudo::new(fake);
        hidden(&s, "0644");
        s.fx.write("rustowroot/etc/hidden/x.conf", content);
        let out = s.run(&["apply", "rustowroot/etc/hidden"]);
        assert_eq!(out.code, 4, "{out:?}");
        let doctor = s.fx.run(&["doctor"]);
        let line = line_of(&doctor, "temp file");
        assert!(
            line.contains("etc/hidden/.rustow-tmp-") && line.contains("remove it with `sudo rm -- "),
            "{doctor:?}"
        );
    }
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

/// A file owner may give its file any of its own groups, so a `group`
/// that the user is a member of needs no sudo in a directory that the
/// user can write: the primary group and, when the user has one, a
/// supplementary group.
#[test]
fn a_group_of_your_own_needs_no_sudo() {
    use nix::unistd::{Gid, Group, getegid};
    let s = Sudo::new(FAKE);
    let mut gids = vec![getegid()];
    let others = rustix::process::getgroups().unwrap().into_iter().map(|g| g.as_raw());
    match others.map(Gid::from_raw).find(|g| *g != getegid()) {
        Some(gid) => gids.push(gid),
        None => eprintln!("SKIP: the user has no supplementary group; only the primary group is tested"),
    }
    let names: Vec<String> = gids
        .iter()
        .filter_map(|g| Group::from_gid(*g).ok().flatten().map(|g| g.name))
        .collect();
    let mut config = String::from("version = 1\n");
    for (i, name) in names.iter().enumerate() {
        config.push_str(&format!(
            "[[files]]\nsource = \"rustowroot/srv/g{i}.conf\"\ngroup = \"{name}\"\n"
        ));
        s.fx.write(&format!("rustowroot/srv/g{i}.conf"), "g\n");
    }
    s.fx.write("rustow.toml", &config);
    fs::create_dir(s.fx.root_path("srv")).unwrap();

    let out = s.run(&["apply", "rustowroot/srv"]);
    assert_eq!(out.code, 0, "{out:?}");
    assert!(!out.stdout.contains("via sudo"), "{out:?}");
    assert_eq!(s.calls(), 0, "{out:?}");
    for (i, gid) in gids.iter().enumerate().take(names.len()) {
        let meta = fs::metadata(s.fx.root_path(&format!("srv/g{i}.conf"))).unwrap();
        assert_eq!(std::os::unix::fs::MetadataExt::gid(&meta), gid.as_raw());
    }
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

/// A system file that you cannot read (for example mode 0640, owner root)
/// is compared with the state. Here the test user owns it with mode 0000,
/// so the fake helper cannot read it either: it is root's view of the
/// plan, without root.
fn unreadable(s: &Sudo, rel: &str) -> PathBuf {
    let path = s.fx.root_path(rel);
    set_mode(&path, 0o000);
    path
}

#[test]
fn a_system_file_you_cannot_read_is_compared_with_the_state() {
    let s = Sudo::new(FAKE);
    assert_eq!(s.run(&["apply"]).code, 0);
    let app = unreadable(&s, "etc/app.conf");

    let status = s.run(&["status", "-v"]);
    assert_eq!(status.code, 0, "{status:?}");
    let line = status
        .stdout
        .lines()
        .find(|l| l.contains("etc/app.conf"))
        .unwrap_or_default();
    assert!(
        line.contains("COPIED") && line.contains("not readable by you; compared with the state"),
        "{status:?}"
    );
    let again = s.run(&["apply"]);
    assert_eq!(again.code, 0, "{again:?}");
    assert_eq!(s.calls(), 1, "nothing to change, no sudo");

    s.fx.write("rustowroot/etc/app.conf", "b\n");
    s.fx.write(
        "rustow.toml",
        &read(&s.fx.repo.join("rustow.toml")).replace("v = \"t\"", "v = \"u\""),
    );
    let dry = s.run(&["-n", "apply"]);
    assert_eq!(dry.code, 0, "{dry:?}");
    let line = dry
        .stdout
        .lines()
        .find(|l| l.contains("etc/app.conf"))
        .unwrap_or_default();
    assert!(line.contains("update") && line.contains("via sudo"), "{dry:?}");

    let diff = s.run(&["diff"]);
    assert_eq!(diff.code, 4, "{diff:?}");
    let why = format!(
        "{}: cannot read the destination (Permission denied); elevated privileges may be required",
        app.display()
    );
    assert!(diff.stderr.contains(&why), "{diff:?}");
    assert!(diff.stdout.contains("+u"), "the other items go on: {diff:?}");

    // The fake helper runs as the user: it cannot check the old content
    // against the state, so it writes nothing.
    let out = s.run(&["apply"]);
    assert_eq!(out.code, 4, "{out:?}");
    assert_eq!(s.calls(), 2, "{out:?}");
    let line = out
        .stdout
        .lines()
        .find(|l| l.contains("etc/app.conf"))
        .unwrap_or_default();
    assert!(line.contains("cannot read"), "{out:?}");
    assert_eq!(read(&s.fx.root_path("etc/t.conf")), "u\n");
    set_mode(&app, 0o644);
    assert_eq!(read(&app), "a\n");
}

/// `diff` takes the decision of the plan, as `status` and `apply` do: an
/// unreadable file that the state shows up to date is not a difference.
#[test]
fn diff_shows_nothing_for_an_unreadable_system_file_that_is_up_to_date() {
    let s = Sudo::new(FAKE);
    assert_eq!(s.run(&["apply"]).code, 0);
    unreadable(&s, "etc/app.conf");

    let diff = s.run(&["diff"]);
    assert_eq!((diff.code, diff.stdout.as_str()), (0, ""), "{diff:?}");
    assert!(!diff.stderr.contains("app.conf"), "{diff:?}");
}

/// The metadata of an unreadable file is known: when only it differs,
/// `diff` shows the metadata line, as for a file that you can read.
#[test]
fn diff_shows_the_metadata_of_an_unreadable_system_file() {
    let s = Sudo::new(FAKE);
    let config = read(&s.fx.repo.join("rustow.toml"));
    s.fx.write(
        "rustow.toml",
        &config.replace("app.conf\"\n", "app.conf\"\nmode = \"0640\"\n"),
    );
    assert_eq!(s.run(&["apply"]).code, 0);
    let app = unreadable(&s, "etc/app.conf");

    let diff = s.run(&["diff"]);
    assert_eq!(diff.code, 1, "{diff:?}");
    let line = format!("{}: metadata mode 0000", app.display());
    assert!(
        diff.stdout.contains(&line) && diff.stdout.contains("-> mode 0640"),
        "{diff:?}"
    );
    assert!(!diff.stderr.contains("app.conf"), "{diff:?}");
}

#[test]
fn an_unreadable_system_file_without_a_record_is_a_conflict() {
    let s = Sudo::new(FAKE);
    set_mode(&s.etc, 0o755);
    fs::write(s.etc.join("app.conf"), "theirs\n").unwrap();
    set_mode(&s.etc, 0o555);
    let app = unreadable(&s, "etc/app.conf");

    let out = s.run(&["apply", "rustowroot/etc/app.conf"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(out.stdout.contains("cannot read the destination"), "{out:?}");
    assert_eq!(s.calls(), 0);
    let forced = s.run(&["-n", "apply", "--force", "rustowroot/etc/app.conf"]);
    assert_eq!(forced.code, 0, "{forced:?}");
    let line = forced
        .stdout
        .lines()
        .find(|l| l.contains("etc/app.conf"))
        .unwrap_or_default();
    assert!(line.contains("replace") && line.contains("via sudo"), "{forced:?}");
    set_mode(&app, 0o644);
    assert_eq!(read(&app), "theirs\n");
}

/// One item that rustow cannot read never stops `undeploy`: the other
/// items go, and that item goes through the helper, which checks it.
#[test]
fn undeploy_goes_on_after_an_unreadable_system_file() {
    let s = Sudo::new(FAKE);
    s.fx.write(".homeconf", "h\n");
    assert_eq!(s.run(&["apply"]).code, 0);
    let app = unreadable(&s, "etc/app.conf");

    let dry = s.run(&["-n", "undeploy"]);
    assert_eq!(dry.code, 0, "{dry:?}");
    let line = dry
        .stdout
        .lines()
        .find(|l| l.contains("etc/app.conf"))
        .unwrap_or_default();
    assert!(line.contains("remove") && line.contains("via sudo"), "{dry:?}");

    let out = s.run(&["undeploy", "--yes"]);
    assert_eq!(out.code, 4, "the fake helper cannot read it: {out:?}");
    assert!(!s.fx.home_path(".homeconf").exists(), "{out:?}");
    assert!(!s.fx.root_path("etc/t.conf").exists(), "{out:?}");
    set_mode(&app, 0o644);
    assert_eq!(read(&app), "a\n", "kept");
}

/// The helper reads, as root, a file that the plan could not read. When
/// its content is not the recorded one, the helper answers with a
/// conflict: the item is a conflict (exit 3), not an error, and the state
/// keeps the old record. Here a fake helper gives that answer.
#[test]
fn a_conflict_that_the_helper_finds_is_a_conflict() {
    let s = Sudo::new(FAKE);
    assert_eq!(s.run(&["apply"]).code, 0);
    let app = unreadable(&s, "etc/app.conf");
    let key = app.to_str().unwrap().to_owned();
    let before = s.fx.state_json()["entries"][&key].clone();
    s.fx.write("rustowroot/etc/app.conf", "b\n");
    let answer = r#"{"created_dirs":[],"removed_dirs":[],"result":{"Err":"destination was modified since rustow last applied it"},"conflict":true}"#;
    fs::write(
        &s.script,
        format!("#!/bin/sh\necho '{READY}'\nIFS= read -r header\nprintf '%s\\n' '{answer}'\nexec cat >/dev/null\n"),
    )
    .unwrap();

    let out = s.run(&["apply", "rustowroot/etc/app.conf"]);
    assert_eq!(out.code, 3, "{out:?}");
    let line = out
        .stdout
        .lines()
        .find(|l| l.contains("etc/app.conf"))
        .unwrap_or_default();
    assert!(
        line.contains("conflict") && line.contains("destination was modified since rustow last applied it"),
        "{out:?}"
    );
    assert_eq!(s.fx.state_json()["entries"][&key], before, "the old record stays");
}
