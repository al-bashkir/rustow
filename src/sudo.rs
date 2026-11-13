//! The sudo helper. rustow runs as the user: the plan, the state, the keys
//! and the cache stay the user's. A system file that the user cannot write
//! (`Deploy::sudo`) goes to `sudo rustow __sudo-helper`, once per run. The
//! helper reads the user's uid and the planned writes as JSON on stdin,
//! runs `execute::write_dest` for each as root, and prints one result per
//! job as JSON on stdout. Its walks apply the trust rule of that user, not
//! root's, and refuse a directory other than the planned one. sudo asks
//! for the password on the terminal.
//! `RUSTOW_SUDO` names another program (`doas`, or a fake in the tests).

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::execute::{self, Policy};
use crate::fsops::{DirId, Observed};
use crate::plan::{Deploy, with_privilege_hint};

/// The hidden command that the helper runs.
pub const HELPER: &str = "__sudo-helper";

/// One destination for the helper.
#[derive(Serialize, Deserialize)]
pub enum Job<D> {
    /// The planned deploy and the conflict policy, with the backup path
    /// the user's run chose.
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

/// The helper's result for one job. The directories it created or removed
/// are listed also when the job failed later.
#[derive(Serialize, Deserialize)]
pub struct Done {
    pub created_dirs: Vec<PathBuf>,
    pub removed_dirs: Vec<PathBuf>,
    pub result: std::result::Result<Option<String>, String>,
}

/// Runs the helper through sudo and returns one result per job.
pub fn call(jobs: &[Job<&Deploy>]) -> Result<Vec<Done>> {
    let (shown, program) = sudo_program()?;
    let exe = std::env::current_exe().map_err(|e| Error::Failed(format!("find the rustow binary: {e}")))?;
    let me = nix::unistd::geteuid().as_raw();
    let input =
        Zeroizing::new(serde_json::to_vec(&(me, jobs)).map_err(|e| Error::Failed(format!("encode sudo jobs: {e}")))?);
    let mut child = Command::new(&program)
        .arg("--")
        .arg(&exe)
        .arg(HELPER)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| Error::Failed(format!("run {shown}: {e}; system files that you cannot write need it")))?;
    let sent = child.stdin.take().map(|mut stdin| stdin.write_all(&input));
    let output = child
        .wait_with_output()
        .map_err(|e| Error::Failed(format!("wait for {shown}: {e}")))?;
    if !output.status.success() {
        return Err(Error::Failed(format!(
            "{shown} failed ({}); system files that you cannot write need it",
            output.status
        )));
    }
    if let Some(Err(e)) = sent {
        return Err(Error::Failed(format!("send the jobs to {shown}: {e}")));
    }
    results(&shown, &output.stdout, jobs.len())
}

/// The helper's output: one result for each of `jobs` jobs.
fn results(shown: &str, stdout: &[u8], jobs: usize) -> Result<Vec<Done>> {
    let done: Vec<Done> =
        serde_json::from_slice(stdout).map_err(|e| Error::Failed(format!("read the result of {shown}: {e}")))?;
    if done.len() != jobs {
        return Err(Error::Failed(format!(
            "{shown} returned {} results for {jobs} files",
            done.len()
        )));
    }
    Ok(done)
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

/// The helper side: reads the user's uid and the jobs on stdin, writes
/// each one, prints the results.
pub fn serve() -> Result<i32> {
    let mut input = Zeroizing::new(Vec::new());
    std::io::stdin()
        .read_to_end(&mut input)
        .map_err(|e| Error::Failed(format!("read the sudo jobs: {e}")))?;
    let (user, jobs): (u32, Vec<Job<Deploy>>) =
        serde_json::from_slice(&input).map_err(|e| Error::Usage(format!("{HELPER} is internal to rustow: {e}")))?;
    let done: Vec<Done> = jobs.iter().map(|job| run(job, user)).collect();
    let text = serde_json::to_string(&done).map_err(|e| Error::Failed(format!("encode the results: {e}")))?;
    crate::report::out(format_args!("{text}"));
    Ok(0)
}

/// Runs one job with the executor's own code, walking with the trust rule
/// of `user`; a relative path is refused.
fn run(job: &Job<Deploy>, user: u32) -> Done {
    let mut done = Done {
        created_dirs: Vec::new(),
        removed_dirs: Vec::new(),
        result: Err("the sudo helper takes absolute paths only".into()),
    };
    let result = match job {
        Job::Write { deploy, policy, backup } if deploy.path.is_absolute() && backup.is_absolute() => {
            execute::write_dest(deploy, *policy, backup, user, &mut done.created_dirs)
        }
        Job::Remove {
            path,
            dir_id,
            observed,
            created_dirs,
        } if path.is_absolute() => {
            execute::remove_dest(path, *dir_id, observed, created_dirs, user, &mut done.removed_dirs).map(|()| None)
        }
        _ => return done,
    };
    done.result = result.map_err(with_privilege_hint);
    done
}
