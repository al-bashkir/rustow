//! The sudo helper. rustow runs as the user: the plan, the state, the keys
//! and the cache stay the user's. A system file that the user cannot write
//! (`Deploy::sudo`) goes to `sudo rustow __sudo-helper`, once per run. The
//! helper reads the planned writes as JSON on stdin, runs
//! `execute::write_dest` for each as root, and prints one result per job
//! as JSON on stdout. sudo asks for the password on the terminal.
//! `RUSTOW_SUDO` names another program (`doas`, or a fake in the tests).

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::execute::{self, Policy};
use crate::fsops::Observed;
use crate::plan::{Deploy, with_privilege_hint};

/// The hidden command that the helper runs.
pub const HELPER: &str = "__sudo-helper";

/// One destination for the helper.
#[derive(Serialize, Deserialize)]
pub enum Job<D> {
    /// The planned deploy and the conflict policy, with the backup path
    /// the user's run chose.
    Write { deploy: D, policy: Policy, backup: PathBuf },
    /// A recorded file that must still look like `observed`, and the
    /// directories above it that rustow created (removed when empty).
    Remove {
        path: PathBuf,
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
    let program = std::env::var_os("RUSTOW_SUDO").unwrap_or_else(|| "sudo".into());
    let shown = PathBuf::from(&program).display().to_string();
    let exe = std::env::current_exe().map_err(|e| Error::Failed(format!("find the rustow binary: {e}")))?;
    let input = Zeroizing::new(serde_json::to_vec(jobs).map_err(|e| Error::Failed(format!("encode sudo jobs: {e}")))?);
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
    let done: Vec<Done> = serde_json::from_slice(&output.stdout)
        .map_err(|e| Error::Failed(format!("read the result of {shown}: {e}")))?;
    if done.len() != jobs.len() {
        return Err(Error::Failed(format!(
            "{shown} returned {} results for {} files",
            done.len(),
            jobs.len()
        )));
    }
    Ok(done)
}

/// The helper side: reads the jobs on stdin, writes each one, prints the results.
pub fn serve() -> Result<i32> {
    let mut input = Zeroizing::new(Vec::new());
    std::io::stdin()
        .read_to_end(&mut input)
        .map_err(|e| Error::Failed(format!("read the sudo jobs: {e}")))?;
    let jobs: Vec<Job<Deploy>> =
        serde_json::from_slice(&input).map_err(|e| Error::Usage(format!("{HELPER} is internal to rustow: {e}")))?;
    let done: Vec<Done> = jobs.iter().map(run).collect();
    let text = serde_json::to_string(&done).map_err(|e| Error::Failed(format!("encode the results: {e}")))?;
    crate::report::out(format_args!("{text}"));
    Ok(0)
}

/// Runs one job with the executor's own code; a relative path is refused.
fn run(job: &Job<Deploy>) -> Done {
    let mut done = Done {
        created_dirs: Vec::new(),
        removed_dirs: Vec::new(),
        result: Err("the sudo helper takes absolute paths only".into()),
    };
    let result = match job {
        Job::Write { deploy, policy, backup } if deploy.path.is_absolute() && backup.is_absolute() => {
            execute::write_dest(deploy, *policy, backup, &mut done.created_dirs)
        }
        Job::Remove {
            path,
            observed,
            created_dirs,
        } if path.is_absolute() => {
            execute::remove_dest(path, observed, created_dirs, &mut done.removed_dirs).map(|()| None)
        }
        _ => return done,
    };
    done.result = result.map_err(with_privilege_hint);
    done
}
