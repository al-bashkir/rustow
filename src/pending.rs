//! The journal of temporary files, `<state_dir>/<repo-id>.pending`. While
//! a command holds the repository lock, `fsops::temp_name` appends the
//! path of each new temporary file to it, one JSON string per line, and
//! syncs it before the file exists. A run that ends has no temporary file
//! left and removes the journal. After a run that died, the next command
//! that takes the lock removes the files that the journal lists, or prints
//! the command that removes them. An entry stays until its file is gone.
//! rustow looks at an entry only through its directory, walked with the
//! trust rule (`fsops::Dir`). rustow never runs sudo only to clean up.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use nix::fcntl::OFlag;
use nix::unistd::geteuid;

use crate::error::{IoContext, Result};
use crate::fsops::{self, Dir, TEMP_PREFIX, Walked};
use crate::report::shown;
use crate::state::MAX_STATE_SIZE;

/// The journal of the run that holds the lock. Dropping it ends the
/// journal: entries whose files are gone go, and so does an empty journal.
pub struct Pending {
    path: PathBuf,
}

/// Temporary names of this run whose files are gone or were never made,
/// also where the user cannot look (`settled`).
static SETTLED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Tells the journal that no file with the temporary name `name` is left:
/// the end of this run drops its entries.
pub fn settled(name: &str) {
    SETTLED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(name.to_owned());
}

/// What a journaled temporary file is now.
#[derive(Clone)]
enum Leftover {
    Gone,
    /// The user may remove it: its directory is writable.
    Removable,
    /// Only root may remove it, for example a file of the sudo helper.
    NeedsRoot,
    /// rustow cannot look: the walk to its directory is blocked, for this reason.
    Unchecked(String),
}

impl Pending {
    /// Starts the journal at `path`. First it removes the temporary files
    /// that an interrupted run left and that the journal lists, and gives
    /// one notice for each, ready to print: the command in a notice must
    /// not go through `shown` again, so the names in it already did.
    pub fn start(path: PathBuf) -> Result<(Pending, Vec<String>)> {
        let mut notices = Vec::new();
        let me = geteuid().as_raw();
        let kept: Vec<PathBuf> = entries(&path)?
            .into_iter()
            .filter(|temp| recover(temp, me, &mut notices))
            .collect();
        rewrite(&path, &kept)?;
        let journal = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(OFlag::O_NOFOLLOW.bits())
            .open(&path)
            .ctx(|| format!("open the journal {}", path.display()))?;
        fsops::journal_to(Some(journal));
        Ok((Pending { path }, notices))
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        fsops::journal_to(None);
        let settled = std::mem::take(&mut *SETTLED.lock().unwrap_or_else(PoisonError::into_inner));
        finish(&self.path, &settled.into_iter().collect());
    }
}

/// Keeps in the journal at `path` only the entries that this run did not
/// settle and whose files are still there. A run journals the files of
/// one directory one after another, so they share one walk to it.
fn finish(path: &Path, settled: &HashSet<String>) {
    let Ok(entries) = entries(path) else { return };
    let me = geteuid().as_raw();
    let mut walked: Option<(PathBuf, std::result::Result<Dir, Leftover>)> = None;
    let mut kept = Vec::new();
    for temp in entries {
        let Ok((parent, name)) = fsops::split(&temp) else {
            continue;
        };
        if name.to_str().is_some_and(|n| settled.contains(n)) {
            continue;
        }
        if walked.as_ref().is_none_or(|(dir, _)| dir != parent) {
            walked = Some((parent.to_path_buf(), walk(parent, me)));
        }
        if let Some((_, dir)) = &walked
            && !matches!(found(dir, name), Leftover::Gone)
        {
            kept.push(temp);
        }
    }
    let _ = rewrite(path, &kept);
}

/// One line for each temporary file that the journal at `path` lists and
/// that may still be there, with what to do about it (for `doctor`).
/// Like the notices of `Pending::start`, each line is ready to print: its
/// names went through `shown`, and its command is a `shell_word`.
pub fn report(path: &Path) -> Result<Vec<String>> {
    let me = geteuid().as_raw();
    Ok(entries(path)?
        .iter()
        .filter_map(|temp| match leftover(temp, me) {
            Leftover::Removable => Some(format!(
                "{}: {LEFT}; the next command that changes files removes it",
                named(temp)
            )),
            other => kept(temp, &other),
        })
        .collect())
}

const LEFT: &str = "a temporary file that an interrupted run left";

/// `path` escaped for a notice (`shown`).
fn named(path: &Path) -> String {
    shown(&path.display().to_string()).into_owned()
}

/// The notice for a leftover that the user cannot remove or cannot look at.
fn kept(temp: &Path, found: &Leftover) -> Option<String> {
    match found {
        Leftover::NeedsRoot => Some(needs_root(temp)),
        Leftover::Unchecked(why) => Some(format!("{}: cannot look for {LEFT}: {}", named(temp), shown(why))),
        Leftover::Gone | Leftover::Removable => None,
    }
}

/// Removes the leftover `temp` when `me` can; true when the journal must keep it.
fn recover(temp: &Path, me: u32, notices: &mut Vec<String>) -> bool {
    match leftover(temp, me) {
        Leftover::Gone => false,
        Leftover::Removable => match remove(temp) {
            Ok(()) => {
                notices.push(format!("removed {}: {LEFT}", named(temp)));
                false
            }
            Err(e) => {
                notices.push(format!(
                    "{}: {LEFT}; cannot remove it: {}",
                    named(temp),
                    shown(&e.to_string())
                ));
                true
            }
        },
        other => {
            notices.extend(kept(temp, &other));
            true
        }
    }
}

/// Removes `temp` through its directory, walked for the current user.
fn remove(temp: &Path) -> Result<()> {
    let (parent, name) = fsops::split(temp)?;
    fsops::open_ready(parent, geteuid().as_raw())?.remove(name)
}

/// What `temp` is now, seen through its directory walked for `me`.
fn leftover(temp: &Path, me: u32) -> Leftover {
    match fsops::split(temp) {
        Ok((parent, name)) => found(&walk(parent, me), name),
        Err(_) => Leftover::Gone,
    }
}

/// The directory `parent`, walked for `me`; else what each file in it is.
fn walk(parent: &Path, me: u32) -> std::result::Result<Dir, Leftover> {
    match Dir::open(parent, me) {
        Ok(Walked::Ready(dir)) => Ok(dir),
        Ok(Walked::Missing { .. }) => Err(Leftover::Gone),
        Ok(Walked::Blocked { path, reason }) => Err(Leftover::Unchecked(format!("{}: {reason}", path.display()))),
        // A directory that the user cannot search: only root can look, as in `found`.
        Ok(Walked::Sealed { .. }) => Err(Leftover::NeedsRoot),
        Err(e) => Err(Leftover::Unchecked(e.to_string())),
    }
}

/// What the file `name` is now, in `dir` (from `walk`).
fn found(dir: &std::result::Result<Dir, Leftover>, name: &OsStr) -> Leftover {
    let dir = match dir {
        Ok(dir) => dir,
        Err(found) => return found.clone(),
    };
    match (dir.may_have(name), dir.writable()) {
        (false, _) => Leftover::Gone,
        (true, true) => Leftover::Removable,
        // Also a file in a directory that the user cannot search: only root can look.
        (true, false) => Leftover::NeedsRoot,
    }
}

fn needs_root(temp: &Path) -> String {
    format!(
        "{}: {LEFT}; remove it with `sudo rm -- {}`",
        named(temp),
        shell_word(temp)
    )
}

/// `path` as one shell word that the shell reads back to the same bytes,
/// and that prints safely as it is: bare when it has only letters, digits
/// and `/._-+:@%,=`; in single quotes when `shown` changes nothing in it;
/// else in `$'...'` (bash, zsh), with `\xHH` for each byte that is not
/// printable ASCII, and for `\` and `'`.
fn shell_word(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    match path.to_str() {
        Some(t) if t.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+:@%,=".contains(c)) => t.to_owned(),
        Some(t) if shown(t) == t => format!("'{}'", t.replace('\'', r"'\''")),
        _ => {
            let bytes = path.as_os_str().as_bytes().iter();
            let body: String = bytes
                .map(|&b| match b {
                    b' '..=b'~' if b != b'\\' && b != b'\'' => char::from(b).to_string(),
                    _ => format!("\\x{b:02x}"),
                })
                .collect();
            format!("$'{body}'")
        }
    }
}

/// The temporary files that the journal at `path` lists; none when there
/// is no journal. A line that is not an absolute path with a temporary
/// name is skipped.
pub fn entries(path: &Path) -> Result<Vec<PathBuf>> {
    let bytes = match fsops::read_limited(path, OFlag::O_NOFOLLOW, MAX_STATE_SIZE) {
        Ok(bytes) => bytes,
        Err(_) if fs::symlink_metadata(path).is_err() => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let temp = |p: &PathBuf| {
        p.is_absolute()
            && p.file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|n| n.starts_with(TEMP_PREFIX))
    };
    Ok(bytes
        .split(|&b| b == b'\n')
        .filter_map(|line| serde_json::from_slice::<PathBuf>(line).ok())
        .filter(temp)
        .collect())
}

/// Writes `kept` as the journal at `path`, or removes the journal when
/// `kept` is empty. Call it only while no journal is set.
fn rewrite(path: &Path, kept: &[PathBuf]) -> Result<()> {
    if kept.is_empty() {
        return match fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e).ctx(|| format!("remove {}", path.display())),
            _ => Ok(()),
        };
    }
    let text: String = kept.iter().map(|p| format!("{}\n", serde_json::json!(p))).collect();
    fsops::write_atomic(path, text.as_bytes(), 0o600, None, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_with_other_characters_is_quoted_for_the_shell() {
        assert_eq!(
            shell_word(Path::new("/etc/.rustow-tmp-1-a-0")),
            "/etc/.rustow-tmp-1-a-0"
        );
        assert_eq!(shell_word(Path::new("/a b/x")), "'/a b/x'");
        assert_eq!(shell_word(Path::new("/it's")), r"'/it'\''s'");
        assert_eq!(shell_word(Path::new("/$(x)")), "'/$(x)'");
        assert_eq!(shell_word(Path::new("/é")), "'/é'");
    }

    /// A path with a character that `shown` escapes goes into `$'...'`
    /// with `\xHH` for each byte that is not plain printable ASCII, so the
    /// printed command is safe and the shell reads the same bytes back.
    #[test]
    fn a_path_that_shown_escapes_is_a_dollar_quoted_word() {
        use std::os::unix::ffi::OsStrExt;
        let cases: [&[u8]; 6] = [
            b"/etc/a\x1bb",
            b"/a\\b",
            b"/a\tb\nc",
            "/a\u{202e}b'c é".as_bytes(),
            b"/bad\xff",
            b"/x\x7f\x01",
        ];
        for bytes in cases {
            let path = Path::new(OsStr::from_bytes(bytes));
            let word = shell_word(path);
            assert!(word.starts_with("$'") && word.ends_with('\''), "{word}");
            assert!(
                word.bytes().all(|b| (b' '..=b'~').contains(&b)),
                "printable ASCII: {word}"
            );
            let Ok(out) = std::process::Command::new("bash")
                .args(["-c", &format!("printf %s {word}")])
                .output()
            else {
                eprintln!("SKIP: no bash");
                return;
            };
            assert_eq!(out.stdout, bytes, "{word}");
        }
        assert_eq!(shell_word(Path::new("/etc/a\x1bb")), r"$'/etc/a\x1bb'");
        assert_eq!(shell_word(Path::new("/it's\t")), r"$'/it\x27s\x09'");
    }

    /// The notice escapes the path of the text, and gives the command as it is.
    #[test]
    fn the_notice_for_root_names_the_path_once_escaped_and_once_quoted() {
        assert_eq!(
            needs_root(Path::new("/etc/a\x1bb/.rustow-tmp-1")),
            format!(r"/etc/a\x1bb/.rustow-tmp-1: {LEFT}; remove it with `sudo rm -- $'/etc/a\x1bb/.rustow-tmp-1'`")
        );
    }

    /// rustow looks at a leftover only through its directory, walked with
    /// the trust rule. Behind a symlink that another user owns (here the
    /// test user's symlink, checked for another user) it does not look:
    /// the notice names the path and the reason, and gives no sudo
    /// command, and the journal keeps the entry.
    #[test]
    fn a_leftover_behind_an_untrusted_symlink_is_not_checked() {
        let me = geteuid().as_raw();
        if me == 0 {
            eprintln!("SKIP: root owns every symlink it creates");
            return;
        }
        let t = tempfile::tempdir().unwrap();
        let d = t.path().canonicalize().unwrap();
        fs::create_dir(d.join("real")).unwrap();
        std::os::unix::fs::symlink(d.join("real"), d.join("link")).unwrap();
        fs::write(d.join("real/.rustow-tmp-1"), "half").unwrap();
        let journal = d.join("x.pending");
        fs::write(
            &journal,
            format!("{}\n", serde_json::json!(d.join("link/.rustow-tmp-1"))),
        )
        .unwrap();
        let temp = &entries(&journal).unwrap()[0];

        let mut notices = Vec::new();
        assert!(recover(temp, me + 1, &mut notices), "kept");
        let why = format!("{}: symlink is not safe", d.join("link").display());
        assert_eq!(notices.len(), 1);
        assert!(
            notices[0].starts_with(&format!("{}: cannot look for", temp.display()))
                && notices[0].contains(&why)
                && !notices[0].contains("sudo"),
            "{notices:?}"
        );
        assert!(d.join("real/.rustow-tmp-1").exists());

        assert!(!recover(temp, me, &mut notices), "removed through a trusted walk");
        assert!(!d.join("real/.rustow-tmp-1").exists(), "{notices:?}");
        assert!(matches!(leftover(temp, me), Leftover::Gone));
    }

    /// The end of a run keeps, in journal order, the entries that the run
    /// did not settle and whose files are still there; an empty journal goes.
    #[test]
    fn the_end_of_a_run_keeps_the_files_still_there() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().canonicalize().unwrap();
        for dir in ["a", "b"] {
            fs::create_dir(d.join(dir)).unwrap();
        }
        let temps = ["a/1", "a/2", "b/3", "a/4", "a/5", "gone/6"].map(|p| d.join(p.replace('/', "/.rustow-tmp-")));
        for i in [0, 2, 3, 4] {
            fs::write(&temps[i], "x").unwrap();
        }
        let journal = d.join("x.pending");
        let lines: String = temps.iter().map(|p| format!("{}\n", serde_json::json!(p))).collect();
        fs::write(&journal, lines).unwrap();
        finish(&journal, &HashSet::from([".rustow-tmp-5".to_owned()]));
        assert_eq!(entries(&journal).unwrap(), [0, 2, 3].map(|i| temps[i].clone()));
        for i in [0, 2, 3] {
            fs::remove_file(&temps[i]).unwrap();
        }
        finish(&journal, &HashSet::new());
        assert!(!journal.exists());
    }

    #[test]
    fn only_absolute_temporary_paths_count() {
        let d = tempfile::tempdir().unwrap();
        let journal = d.path().join("x.pending");
        assert!(entries(&journal).unwrap().is_empty(), "no journal, no entry");
        let lines = "\"/a/.rustow-tmp-1\"\n\"rel/.rustow-tmp-2\"\n\"/etc/passwd\"\nnot json\n\"/b/.rustow-tmp-3\"";
        fs::write(&journal, lines).unwrap();
        let want: Vec<PathBuf> = vec!["/a/.rustow-tmp-1".into(), "/b/.rustow-tmp-3".into()];
        assert_eq!(entries(&journal).unwrap(), want);
    }
}
