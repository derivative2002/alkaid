use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

/// A statusLine command must never consume the refresh interval itself.
pub const STATUSLINE_COMMAND_BUDGET: Duration = Duration::from_secs(2);

#[derive(Debug, PartialEq, Eq)]
pub struct CommandOutput {
    pub stdout: Vec<u8>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
}

/// Run a user-owned shell command with a hard wall-clock budget.
///
/// The child owns a process group so a timeout removes the shell and any
/// helper it started. stdout is drained concurrently, while the caller keeps
/// ownership of the child and can therefore kill and reap it without a pid
/// reuse race.
pub fn run_shell_with_deadline(
    command: &str,
    input: &[u8],
    budget: Duration,
) -> Result<CommandOutput> {
    let mut child = Command::new("sh");
    child
        .args(["-c", command])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    unsafe {
        child.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = child.spawn().context("run previous statusLine")?;
    let stdin = child
        .stdin
        .take()
        .context("open previous statusLine stdin")?;
    let stdout = child
        .stdout
        .take()
        .context("open previous statusLine stdout")?;
    let input = input.to_vec();
    let writer = thread::spawn(move || {
        let mut stdin = stdin;
        let _ = stdin.write_all(&input);
    });

    let tree = Tree::contain(&child);
    let child = Arc::new(Mutex::new(Some((child, tree))));
    let timed_out = Arc::new(AtomicBool::new(false));
    let (cancel, cancelled) = mpsc::channel();
    let watchdog_child = Arc::clone(&child);
    let watchdog_timed_out = Arc::clone(&timed_out);
    let watchdog = thread::spawn(move || {
        if cancelled.recv_timeout(budget).is_err() {
            watchdog_timed_out.store(true, Ordering::Release);
            let _ = terminate_and_reap(&watchdog_child);
        }
    });

    let mut stdout = stdout;
    let mut output = Vec::new();
    let read_result = stdout.read_to_end(&mut output);
    let _ = cancel.send(());
    let status = terminate_and_reap(&child)?;
    let _ = watchdog.join();

    let _ = writer.join();
    read_result.context("read previous statusLine output")?;

    Ok(CommandOutput {
        stdout: output,
        exit_code: status.and_then(|status| status.code()),
        timed_out: timed_out.load(Ordering::Acquire),
    })
}

fn terminate_and_reap(child: &Mutex<Option<(Child, Tree)>>) -> Result<Option<ExitStatus>> {
    let mut slot = child
        .lock()
        .map_err(|_| anyhow::anyhow!("lock previous statusLine child"))?;
    let Some((mut child, tree)) = slot.take() else {
        return Ok(None);
    };
    tree.end(&mut child);
    child.wait().map(Some).context("reap previous statusLine")
}

/// Everything a child we started went on to start, so it can be ended as
/// one. On Unix the child was put in its own process group before exec
/// (`setpgid`). On Windows there are no process groups, and Git's MSYS `sh`
/// re-parents a background job to an intermediate process, so walking the
/// parent chain (`taskkill /T`) misses it (measured 1001). A job object does
/// not: every process the child creates joins it. A process the child starts
/// before it is assigned would escape; a shell has not started anything yet
/// in that window.
pub(crate) struct Tree {
    #[cfg(windows)]
    job: Option<windows_job::Job>,
}

impl Tree {
    pub(crate) fn contain(child: &Child) -> Self {
        #[cfg(windows)]
        {
            Tree {
                job: windows_job::Job::containing(child),
            }
        }
        #[cfg(not(windows))]
        {
            let _ = child;
            Tree {}
        }
    }

    /// End the child and its descendants. The child is still held and not
    /// yet reaped, so its pid cannot belong to anything else.
    pub(crate) fn end(&self, child: &mut Child) {
        #[cfg(unix)]
        unsafe {
            let _ = libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
        }
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.terminate();
        }
        let _ = child.kill();
    }
}

#[cfg(windows)]
mod windows_job {
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// A job that ends its processes when terminated or when the last
    /// handle to it closes.
    pub(crate) struct Job(HANDLE);

    // The handle is only used through the kernel calls below, which are safe
    // from any thread.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub(crate) fn containing(child: &Child) -> Option<Self> {
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return None;
                }
                let job = Job(handle);
                let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let set = SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    &limits as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if set == 0 || AssignProcessToJobObject(job.0, child.as_raw_handle() as HANDLE) == 0
                {
                    return None;
                }
                Some(job)
            }
        }

        pub(crate) fn terminate(&self) {
            unsafe {
                TerminateJobObject(self.0, 1);
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn captures_a_completed_command() {
        let result =
            run_shell_with_deadline("printf done", b"ignored", Duration::from_secs(1)).unwrap();
        assert_eq!(result.stdout, b"done");
        assert_eq!(result.exit_code, Some(0));
        assert!(!result.timed_out);
    }

    #[test]
    fn kills_a_command_that_exceeds_its_budget() {
        let started = Instant::now();
        let result =
            run_shell_with_deadline("sleep 20 & wait", b"", Duration::from_millis(100)).unwrap();
        assert!(result.timed_out);
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
