//! The sudo helper. rustow runs as the user: the plan, the state, the keys
//! and the cache stay the user's. A system file that the user cannot write
//! (`Deploy::sudo`) goes to `sudo rustow __sudo-helper`, once per run.
//! sudo asks for the password on the terminal.
//! `RUSTOW_SUDO` names another program (`doas`, or a fake in the tests).
//!
//! The protocol is lock-step frames. rustow writes one frame per job on the
//! helper's stdin: a `Header` as one JSON line, then exactly `len` bytes of
//! content. The helper runs the job with `execute::write_dest` or
//! `execute::remove_dest` as root, and answers with one `Done` JSON line on
//! stdout before rustow sends the next frame. So rustow counts each result
//! as it arrives, and the helper stops after the job whose result it cannot
//! write. Its walks apply the trust rule of the header's user, not root's,
//! and refuse a directory other than the planned one.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::execute::{self, Policy};
use crate::fsops::{DirId, Observed};
use crate::hash::Hash;
use crate::plan::{Deploy, with_privilege_hint};

/// The hidden command that the helper runs.
pub const HELPER: &str = "__sudo-helper";

/// The longest header line that the helper reads.
const MAX_HEADER: usize = 1 << 20;

/// One destination for the helper.
#[derive(Serialize, Deserialize)]
pub enum Job<D> {
    /// The planned deploy and the conflict policy, with the backup path
    /// the user's run chose. The content follows the header.
    Write { deploy: D, policy: Policy, backup: PathBuf },
    /// A recorded file that must still look like `observed` in its planned
    /// directory `dir_id`, and the directories above it that rustow
    /// created (removed when empty).
    Remove {
        path: PathBuf,
        dir_id: DirId,
        observed: Observed,
        created_dirs: BTreeSet<PathBuf>,
    },
}

/// The first line of a frame: the uid of the user whose trust rule the
/// helper applies, the number of content bytes after this line, and the job.
#[derive(Serialize, Deserialize)]
pub struct Header<J> {
    pub user: u32,
    pub len: u64,
    pub job: J,
}

/// The helper's result for one job. The directories it created or removed
/// are listed also when the job failed later.
#[derive(Serialize, Deserialize)]
pub struct Done {
    pub created_dirs: Vec<PathBuf>,
    pub removed_dirs: Vec<PathBuf>,
    pub result: std::result::Result<Option<String>, String>,
}

impl Done {
    fn failed(why: String) -> Done {
        Done {
            created_dirs: Vec::new(),
            removed_dirs: Vec::new(),
            result: Err(why),
        }
    }
}

/// Runs the helper through sudo and returns one result per job. A job
/// without a result line fails with the reason: an interrupt, or the exit
/// status of sudo.
pub fn call(jobs: &[Job<&Deploy>], stop: &AtomicBool) -> Vec<Done> {
    let mut done = Vec::with_capacity(jobs.len());
    if let Err(e) = exchange(jobs, stop, &mut done) {
        done.resize_with(jobs.len(), || Done::failed(e.to_string()));
    }
    done
}

/// Starts the helper and sends the jobs in lock-step. Err explains the
/// jobs that have no result in `done`.
fn exchange(jobs: &[Job<&Deploy>], stop: &AtomicBool, done: &mut Vec<Done>) -> Result<()> {
    if stop.load(Ordering::SeqCst) {
        return Err(Error::Failed("interrupted before sudo; nothing changed".into()));
    }
    let (shown, mut child) = start()?;
    if let (Some(to), Some(from)) = (child.stdin.take(), child.stdout.take()) {
        lock_step(jobs, stop, to, &mut BufReader::new(from), done);
    }
    let status = child
        .wait()
        .map_err(|e| Error::Failed(format!("wait for {shown}: {e}")))?;
    match (done.len() == jobs.len(), stop.load(Ordering::SeqCst)) {
        (true, _) => Ok(()),
        (false, true) => Err(Error::Failed("interrupted before this item; nothing changed".into())),
        (false, false) => Err(Error::Failed(format!(
            "{shown} failed ({status}); system files that you cannot write need it"
        ))),
    }
}

/// `sudo -- <rustow> __sudo-helper` with pipes on stdin and stdout, and
/// the name of the sudo program for messages.
fn start() -> Result<(String, Child)> {
    let (shown, program) = sudo_program()?;
    let exe = std::env::current_exe().map_err(|e| Error::Failed(format!("find the rustow binary: {e}")))?;
    let child = Command::new(&program)
        .arg("--")
        .arg(&exe)
        .arg(HELPER)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| Error::Failed(format!("run {shown}: {e}; system files that you cannot write need it")))?;
    Ok((shown, child))
}

/// Sends each frame and reads its result line, until an interrupt or until
/// the helper stops answering. Dropping `to` at the end tells the helper
/// that no job follows.
fn lock_step(
    jobs: &[Job<&Deploy>],
    stop: &AtomicBool,
    mut to: ChildStdin,
    from: &mut impl BufRead,
    done: &mut Vec<Done>,
) {
    let user = nix::unistd::geteuid().as_raw();
    for job in jobs {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        // A source that changed since the plan fails its job here; it is not sent.
        let (mut content, len) = match content_of(job) {
            Ok(opened) => opened,
            Err(e) => {
                done.push(Done::failed(with_privilege_hint(e)));
                continue;
            }
        };
        if send(&mut to, Header { user, len, job }, &mut content).is_err() {
            return;
        }
        match receive(from) {
            Some(result) => done.push(result),
            None => return,
        }
    }
}

/// The content of a job and its length: for a write, what
/// `Deploy::open_content` reads; nothing for a removal.
fn content_of<'a>(job: &Job<&'a Deploy>) -> Result<(Box<dyn Read + 'a>, u64)> {
    match job {
        Job::Write { deploy, .. } => deploy.open_content(),
        Job::Remove { .. } => Ok((Box::new(io::empty()), 0)),
    }
}

/// Writes the frame of a job: its header line, then `header.len` bytes of
/// `content`. Content that ends early is filled with zero bytes, so the
/// helper stays in step; its hash check then fails the job.
fn send(to: &mut impl Write, header: Header<&Job<&Deploy>>, content: &mut dyn Read) -> io::Result<()> {
    let mut line = Zeroizing::new(serde_json::to_vec(&header)?);
    line.push(b'\n');
    to.write_all(&line)?;
    let mut filled = content.chain(io::repeat(0)).take(header.len);
    Hash::copy(&mut filled, header.len, to)?;
    to.flush()
}

/// The next result line of the helper; `None` at the end of its output,
/// or for a line that is not a result.
fn receive(from: &mut impl BufRead) -> Option<Done> {
    let mut line = String::new();
    match from.read_line(&mut line) {
        Ok(n) if n > 0 => serde_json::from_str(&line).ok(),
        _ => None,
    }
}

/// `RUSTOW_SUDO`, else `sudo`: its name for messages, and the path to start.
fn sudo_program() -> Result<(String, PathBuf)> {
    let name = std::env::var_os("RUSTOW_SUDO").unwrap_or_else(|| "sudo".into());
    let shown = PathBuf::from(&name).display().to_string();
    match crate::fsops::find_program(&name) {
        Some(path) => Ok((shown, path)),
        None => Err(Error::Failed(format!(
            "{shown}: not found in PATH; system files that you cannot write need it"
        ))),
    }
}

/// The helper side: serves the frames of stdin. It reads stdin without a
/// buffer of its own, so that a header read takes no content byte with it.
pub fn serve() -> Result<i32> {
    let stdin = std::io::stdin()
        .as_fd()
        .try_clone_to_owned()
        .map_err(|e| Error::Failed(format!("read the sudo jobs: {e}")))?;
    serve_on(&mut File::from(stdin), &mut std::io::stdout().lock())
}

/// Runs the job of each frame of `input` and writes its result as one line
/// to `output`. It stops at the end of `input`, or after the job whose
/// result it cannot write: rustow is gone, and no later job may run.
pub fn serve_on(input: &mut impl Read, output: &mut impl Write) -> Result<i32> {
    while let Some(header) = read_header(input)? {
        let mut body = input.by_ref().take(header.len);
        let done = run(header, &mut body);
        // The job may stop early: skip the rest, so that the next header starts here.
        io::copy(&mut body, &mut io::sink()).map_err(|e| Error::Failed(format!("read the sudo jobs: {e}")))?;
        let mut line = serde_json::to_vec(&done).map_err(|e| Error::Failed(format!("encode the result: {e}")))?;
        line.push(b'\n');
        if output.write_all(&line).and_then(|()| output.flush()).is_err() {
            break;
        }
    }
    Ok(0)
}

/// The next frame header, read one byte at a time up to its newline;
/// `None` at the end of the input.
fn read_header(input: &mut impl Read) -> Result<Option<Header<Job<Deploy>>>> {
    let internal = |why: &str| Error::Usage(format!("{HELPER} is internal to rustow: {why}"));
    let mut line = Zeroizing::new(Vec::new());
    // A buffer of one byte: the header never takes a content byte with it.
    for byte in BufReader::with_capacity(1, input.by_ref()).bytes() {
        match byte.map_err(|e| Error::Failed(format!("read the sudo jobs: {e}")))? {
            b'\n' => {
                return serde_json::from_slice(&line)
                    .map(Some)
                    .map_err(|e| internal(&e.to_string()));
            }
            _ if line.len() == MAX_HEADER => return Err(internal("a job header is longer than 1 MiB")),
            b => line.push(b),
        }
    }
    match line.is_empty() {
        true => Ok(None),
        false => Err(internal("the input ends inside a job header")),
    }
}

/// Runs one job with the executor's own code, walking with the trust rule
/// of the header's user; a relative path is refused. A write streams its
/// content from `body`.
fn run(header: Header<Job<Deploy>>, body: &mut dyn Read) -> Done {
    let user = header.user;
    let mut done = Done::failed("the sudo helper takes absolute paths only".into());
    let result = match header.job {
        Job::Write { deploy, policy, backup } if deploy.path.is_absolute() && backup.is_absolute() => {
            let content = (body, header.len);
            execute::write_dest(&deploy, content, policy, &backup, user, &mut done.created_dirs)
        }
        Job::Remove {
            path,
            dir_id,
            observed,
            created_dirs,
        } if path.is_absolute() => {
            execute::remove_dest(&path, dir_id, &observed, &created_dirs, user, &mut done.removed_dirs).map(|()| None)
        }
        _ => return done,
    };
    done.result = result.map_err(with_privilege_hint);
    done
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsops::{self, FileMeta};
    use crate::hash::Hash;
    use crate::plan::Desired;
    use std::fs;
    use std::path::Path;

    /// An output that takes `lines` lines, then fails as a closed pipe does.
    struct Breaks {
        lines: usize,
        got: Vec<u8>,
    }

    impl Write for Breaks {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.got.iter().filter(|&&b| b == b'\n').count() >= self.lines {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            self.got.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn me() -> u32 {
        nix::unistd::geteuid().as_raw()
    }

    fn tmp() -> (tempfile::TempDir, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().canonicalize().unwrap();
        (t, p)
    }

    /// Appends the frame of `job` to `input`, as rustow sends it.
    fn frame(input: &mut Vec<u8>, job: &Job<&Deploy>) {
        let (mut content, len) = content_of(job).unwrap();
        let header = Header { user: me(), len, job };
        send(input, header, &mut content).unwrap();
    }

    /// A job that removes the file `path`, as planned now.
    fn remove(path: &Path) -> Job<&'static Deploy> {
        let parent = nix::sys::stat::stat(path.parent().unwrap()).unwrap();
        Job::Remove {
            path: path.to_path_buf(),
            dir_id: DirId {
                dev: parent.st_dev,
                ino: parent.st_ino,
            },
            observed: fsops::inspect(path).unwrap(),
            created_dirs: BTreeSet::new(),
        }
    }

    /// The result of job 2 cannot be written, so job 3 never runs.
    #[test]
    fn the_helper_stops_after_the_result_it_cannot_write() {
        let (_t, d) = tmp();
        let mut input = Vec::new();
        for name in ["a", "b", "c"] {
            fs::write(d.join(name), name).unwrap();
            frame(&mut input, &remove(&d.join(name)));
        }
        let mut output = Breaks {
            lines: 1,
            got: Vec::new(),
        };
        assert_eq!(serve_on(&mut input.as_slice(), &mut output).unwrap(), 0);
        assert_eq!(output.got.iter().filter(|&&b| b == b'\n').count(), 1);
        assert!(!d.join("a").exists() && !d.join("b").exists());
        assert!(d.join("c").exists(), "job 3 never ran");
    }

    /// Input that ends inside the content fails the job before anything
    /// is written, and leaves no temporary file.
    #[test]
    fn a_truncated_frame_leaves_no_file_and_no_temp() {
        let (_t, d) = tmp();
        let file = Desired::File {
            hash: Hash::of(b"new content"),
            meta: FileMeta::default(),
        };
        let deploy = crate::execute::tests::create(&d.join("f"), file, Some(b"new content"));
        let job = Job::Write {
            deploy: &deploy,
            policy: Policy::Abort,
            backup: d.join("bk"),
        };
        let mut input = Vec::new();
        frame(&mut input, &job);
        input.truncate(input.len() - 4);
        let mut output = Vec::new();
        serve_on(&mut input.as_slice(), &mut output).unwrap();
        let done: Done = serde_json::from_slice(&output).unwrap();
        assert!(done.result.is_err());
        assert_eq!(fs::read_dir(&d).unwrap().count(), 0, "no file and no temp");
    }

    /// Frames follow each other directly: each header starts where the
    /// content of the one before ends, also when a job reads nothing.
    #[test]
    fn frames_stay_in_step() {
        let (_t, d) = tmp();
        let file = Desired::File {
            hash: Hash::of(b"one"),
            meta: FileMeta::default(),
        };
        let deploy = crate::execute::tests::create(&d.join("f"), file, Some(b"one"));
        let mut input = Vec::new();
        let job = Job::Write {
            deploy: &deploy,
            policy: Policy::Abort,
            backup: "relative".into(),
        };
        frame(&mut input, &job);
        fs::write(d.join("a"), "a").unwrap();
        frame(&mut input, &remove(&d.join("a")));
        let mut output = Vec::new();
        serve_on(&mut input.as_slice(), &mut output).unwrap();
        let lines: Vec<Done> = output
            .split(|&b| b == b'\n')
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_slice(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].result.as_ref().unwrap_err().contains("absolute paths only"));
        assert!(lines[1].result.is_ok() && !d.join("a").exists());
    }
}
