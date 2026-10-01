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

/// A containment step that did not happen, and why.
///
/// The old shape returned `None` and carried on, so a job that was never
/// established looked exactly like one that was: the comment still promised a
/// whole-tree budget while `end` could only kill the root process (Codex g1
/// R2b). The stage names the call and `code` is its `GetLastError`, so the
/// failure can be acted on instead of guessed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JobFailure {
    pub stage: &'static str,
    pub code: Option<i32>,
}

impl std::fmt::Display for JobFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} failed", self.stage)?;
        match self.code {
            Some(code) => write!(formatter, " (win32 error {code})"),
            None => Ok(()),
        }
    }
}

impl std::error::Error for JobFailure {}

/// Run a user-owned shell command with a hard wall-clock budget.
///
/// The child owns a process group, so a timeout removes the shell and any
/// helper it started. stdout is drained concurrently, while the caller keeps
/// ownership of the child and can therefore kill and reap it without a pid
/// reuse race. The child is constrained *before* it runs: on Windows it is
/// created suspended, put in its job, and only then resumed, and a job that
/// cannot be established refuses the command instead of running it
/// unconstrained ([`spawn_constrained`]).
pub fn run_shell_with_deadline(
    command: &str,
    input: &[u8],
    budget: Duration,
) -> Result<CommandOutput> {
    let mut command_line = Command::new("sh");
    command_line
        .args(["-c", command])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    unsafe {
        command_line.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let (mut child, tree) = spawn_constrained(&mut command_line)?;
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

/// Start `command` with its whole tree constrained from the first
/// instruction.
///
/// On Windows the child is created suspended, assigned to its job object, and
/// only then resumed. Creating it first and assigning afterwards leaves a
/// window in which the child has already run: a command that starts a
/// grandchild before the assignment hands that grandchild stdout and stdin,
/// and terminating the job afterwards cannot reach it, so the "hard" deadline
/// is not one (Codex g1 R2a). When the job cannot be established at all, the
/// command is killed while still suspended and the failure is returned: a
/// silently unconstrained run would be worse than no run (Codex g1 R2b).
pub(crate) fn spawn_constrained(command: &mut Command) -> Result<(Child, Tree)> {
    platform::prepare(command);
    let mut child = command.spawn().context("spawn child")?;
    let tree = match contain(&child) {
        Ok(tree) => tree,
        Err(failure) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(anyhow::anyhow!("{failure}"));
        }
    };
    if let Err(failure) = platform::resume(&child) {
        let _ = tree.end(&mut child);
        let _ = child.kill();
        let _ = child.wait();
        return Err(anyhow::anyhow!("{failure}"));
    }
    Ok((child, tree))
}

/// Establish the child's containment. Split out so the failure branches can
/// be exercised without a Windows box (Codex g1 R2b).
fn contain(child: &Child) -> Result<Tree, JobFailure> {
    #[cfg(test)]
    if let Some(injected) = tests::injected_contain() {
        return injected(child);
    }
    platform::contain(child)
}

fn terminate_and_reap(child: &Mutex<Option<(Child, Tree)>>) -> Result<Option<ExitStatus>> {
    let mut slot = child
        .lock()
        .map_err(|_| anyhow::anyhow!("lock previous statusLine child"))?;
    let Some((mut child, tree)) = slot.take() else {
        return Ok(None);
    };
    tree.end(&mut child)
        .context("end previous statusLine tree")?;
    child.wait().map(Some).context("reap previous statusLine")
}

/// Everything a child we started went on to start, so it can be ended as
/// one. On Unix the child was put in its own process group before exec
/// (`setpgid`). On Windows there are no POSIX process groups, and Git's MSYS
/// `sh` re-parents a background job to an intermediate process, so walking
/// the parent chain (`taskkill /T`) misses it (measured 1001). A job object
/// does not: every process the child creates joins it, and the child is
/// already a member before it runs ([`spawn_constrained`]).
pub(crate) struct Tree {
    #[cfg(windows)]
    job: Option<windows_job::Job>,
}

impl Tree {
    /// Containment that constrains nothing: Unix relies on the child's own
    /// process group (`setpgid` ran in the child), so there is no job to
    /// establish and nothing that can fail to.
    #[cfg(not(windows))]
    fn unconstrained() -> Self {
        Tree {}
    }

    /// End the child and its descendants. The child is still held and not
    /// yet reaped, so its pid cannot belong to anything else. A termination
    /// that fails is reported rather than swallowed: the caller promised the
    /// subtree is gone, and it is not.
    pub(crate) fn end(&self, child: &mut Child) -> Result<()> {
        #[cfg(unix)]
        unsafe {
            let _ = libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
        }
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.terminate().context("terminate the child's job")?;
        }
        let _ = child.kill();
        Ok(())
    }
}

/// The platform's containment, kept in one place so the shared code above
/// says what is guaranteed and not how.
#[cfg(not(windows))]
mod platform {
    use super::*;

    pub(super) fn prepare(_command: &mut Command) {}

    pub(super) fn contain(_child: &Child) -> Result<Tree, JobFailure> {
        Ok(Tree::unconstrained())
    }

    /// Nothing was suspended, so there is nothing to resume.
    pub(super) fn resume(_child: &Child) -> Result<(), JobFailure> {
        Ok(())
    }
}

#[cfg(windows)]
mod platform {
    use super::{JobFailure, Tree};
    use std::process::Child;
    use windows_sys::Win32::Foundation::{GetLastError, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::Threading::{
        OpenThread, ResumeThread, CREATE_SUSPENDED, THREAD_SUSPEND_RESUME,
    };

    pub(super) fn prepare(command: &mut impl std::os::windows::process::CommandExt) {
        // A suspended child has not executed one instruction when it is
        // assigned, so no descendant can exist yet.
        command.creation_flags(CREATE_SUSPENDED);
    }

    pub(super) fn contain(child: &Child) -> Result<Tree, JobFailure> {
        Tree::contained(child)
    }

    pub(super) fn resume(child: &Child) -> Result<(), JobFailure> {
        let pid = child.id();
        let tid = primary_thread(pid).ok_or_else(|| JobFailure {
            stage: "CreateToolhelp32Snapshot",
            code: last_error(),
        })?;
        // SAFETY: the handle is closed once, on every path below.
        let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, tid) };
        if thread.is_null() {
            return Err(JobFailure {
                stage: "OpenThread",
                code: last_error(),
            });
        }
        let resumed = unsafe { ResumeThread(thread) };
        unsafe { windows_sys::Win32::Foundation::CloseHandle(thread) };
        if resumed == u32::MAX {
            return Err(JobFailure {
                stage: "ResumeThread",
                code: last_error(),
            });
        }
        Ok(())
    }

    /// The one thread a just-created process has. It is still suspended, so
    /// nothing else can have resumed or replaced it.
    fn primary_thread(pid: u32) -> Option<u32> {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut found = None;
        unsafe {
            if Thread32First(snapshot, &mut entry) != 0 {
                loop {
                    if entry.th32OwnerProcessID == pid {
                        found = Some(entry.th32ThreadID);
                        break;
                    }
                    if Thread32Next(snapshot, &mut entry) == 0 {
                        break;
                    }
                }
            }
            windows_sys::Win32::Foundation::CloseHandle(snapshot);
        }
        found
    }

    fn last_error() -> Option<i32> {
        Some(unsafe { GetLastError() } as i32)
    }
}

#[cfg(windows)]
mod windows_job {
    use super::JobFailure;
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
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
        /// Every step that can fail reports which one it was, and closes the
        /// handle it opened: a partially built job must not leak.
        pub(crate) fn build(child: &Child) -> Result<Self, JobFailure> {
            let job = Self::create()?;
            if let Err(failure) = job.kill_with_its_last_handle() {
                return Err(failure);
            }
            if let Err(failure) = job.assign(child) {
                return Err(failure);
            }
            Ok(job)
        }

        fn create() -> Result<Self, JobFailure> {
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(JobFailure {
                    stage: "CreateJobObjectW",
                    code: win_error(),
                });
            }
            Ok(Job(handle))
        }

        fn kill_with_its_last_handle(&self) -> Result<(), JobFailure> {
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set = unsafe {
                SetInformationJobObject(
                    self.0,
                    JobObjectExtendedLimitInformation,
                    &limits as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if set == 0 {
                return Err(JobFailure {
                    stage: "SetInformationJobObject",
                    code: win_error(),
                });
            }
            Ok(())
        }

        fn assign(&self, child: &Child) -> Result<(), JobFailure> {
            if unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle() as HANDLE) } == 0 {
                return Err(JobFailure {
                    stage: "AssignProcessToJobObject",
                    code: win_error(),
                });
            }
            Ok(())
        }

        pub(crate) fn terminate(&self) -> Result<(), JobFailure> {
            if unsafe { TerminateJobObject(self.0, 1) } == 0 {
                return Err(JobFailure {
                    stage: "TerminateJobObject",
                    code: win_error(),
                });
            }
            Ok(())
        }
    }

    fn win_error() -> Option<i32> {
        Some(unsafe { GetLastError() } as i32)
    }

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(windows)]
impl Tree {
    fn contained(child: &Child) -> Result<Self, JobFailure> {
        Ok(Tree {
            job: Some(windows_job::Job::build(child)?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::time::Instant;

    /// How a test replaces containment for the duration of its own thread.
    ///
    /// Per thread, not process-global: the code under test calls `contain`
    /// itself, so a lock held across the call would deadlock, and a shared
    /// slot would leak the double into every other test running beside it.
    pub(super) type Contain = fn(&Child) -> Result<Tree, JobFailure>;

    thread_local! {
        static INJECTED_CONTAIN: Cell<Option<Contain>> = const { Cell::new(None) };
    }

    pub(super) fn injected_contain() -> Option<Contain> {
        INJECTED_CONTAIN.with(Cell::get)
    }

    fn inject(contain: Contain) {
        INJECTED_CONTAIN.with(|slot| slot.set(Some(contain)));
    }

    fn clear() {
        INJECTED_CONTAIN.with(|slot| slot.set(None));
    }

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

    #[test]
    fn a_grandchild_started_at_spawn_is_collected_by_the_budget() {
        // Codex g1 R2a, as a behaviour rather than a claim about timing. The
        // shell starts `sleep` and exits at once; the grandchild inherits
        // stdout, so a tree that is not constrained from the first
        // instruction cannot be ended: reading the pipe waits out the whole
        // thirty seconds.
        let started = Instant::now();
        let result =
            run_shell_with_deadline("sleep 30 &", b"", Duration::from_millis(200)).unwrap();
        assert!(
            result.timed_out,
            "{}",
            String::from_utf8_lossy(&result.stdout)
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the grandchild outlived the budget: {:?}",
            started.elapsed()
        );
    }

    /// The same guarantee for a grandchild that does *not* hold the pipe, so
    /// the call returns normally: it must still be gone once the call has
    /// ended its tree. Windows-only, because the marker costs a few seconds
    /// and the Unix process group has covered this since before the job
    /// object existed.
    #[cfg(windows)]
    #[test]
    fn a_grandchild_started_at_spawn_does_not_outlive_the_call() {
        let marker = std::path::Path::new("alkaid-escaped-grandchild.marker");
        let _ = std::fs::remove_file(marker);
        let result = run_shell_with_deadline(
            "(sleep 2; : > alkaid-escaped-grandchild.marker) </dev/null >/dev/null 2>&1 & echo started",
            b"",
            Duration::from_secs(2),
        )
        .unwrap();
        assert!(
            String::from_utf8_lossy(&result.stdout).contains("started"),
            "{}",
            String::from_utf8_lossy(&result.stdout)
        );
        thread::sleep(Duration::from_secs(4));
        assert!(
            !marker.exists(),
            "a grandchild ran on after the call ended the tree it promised to end"
        );
    }

    /// Each step of containment, refused on its own (Codex g1 R2b).
    fn refuses_at(stage: &'static str) -> Contain {
        fn create(_child: &Child) -> Result<Tree, JobFailure> {
            Err(JobFailure {
                stage: "CreateJobObjectW",
                code: Some(5),
            })
        }
        fn limit(_child: &Child) -> Result<Tree, JobFailure> {
            Err(JobFailure {
                stage: "SetInformationJobObject",
                code: Some(87),
            })
        }
        fn assign(_child: &Child) -> Result<Tree, JobFailure> {
            Err(JobFailure {
                stage: "AssignProcessToJobObject",
                code: Some(5),
            })
        }
        match stage {
            "CreateJobObjectW" => create,
            "SetInformationJobObject" => limit,
            _ => assign,
        }
    }

    #[test]
    fn a_command_whose_tree_cannot_be_constrained_is_refused() {
        // The caller must not get a child back when containment failed: on
        // Windows the child is still suspended at this point, so refusing
        // costs no user code, and the alternative — an unconstrained run
        // reported as a bounded one — is what Codex g1 R2b rejects.
        for stage in [
            "CreateJobObjectW",
            "SetInformationJobObject",
            "AssignProcessToJobObject",
        ] {
            inject(refuses_at(stage));
            let error =
                run_shell_with_deadline("printf x", b"", Duration::from_secs(2)).unwrap_err();
            clear();
            assert!(error.to_string().contains(stage), "{stage}: {error}");
        }
    }

    #[test]
    fn a_failed_containment_names_the_step_and_its_error() {
        // What the caller and the recorded provider error get to read.
        assert_eq!(
            JobFailure {
                stage: "AssignProcessToJobObject",
                code: Some(5),
            }
            .to_string(),
            "AssignProcessToJobObject failed (win32 error 5)"
        );
        assert_eq!(
            JobFailure {
                stage: "TerminateJobObject",
                code: None,
            }
            .to_string(),
            "TerminateJobObject failed"
        );
        let error = run_shell_with_deadline_with(refuses_at("AssignProcessToJobObject"));
        assert_eq!(
            error, "AssignProcessToJobObject failed (win32 error 5)",
            "the whole chain must reach the caller, not just a stage name"
        );
    }

    fn run_shell_with_deadline_with(contain: Contain) -> String {
        inject(contain);
        let error = run_shell_with_deadline("printf x", b"", Duration::from_secs(2)).unwrap_err();
        clear();
        // The error tunnel through `spawn_constrained`'s anyhow wrapper.
        format!("{error:#}")
    }

    #[test]
    fn a_constrained_child_still_reports_its_output_and_status() {
        inject(|child| Ok(conforming_tree(child)));
        let result =
            run_shell_with_deadline("printf ok; exit 3", b"", Duration::from_secs(2)).unwrap();
        clear();
        assert_eq!(result.stdout, b"ok");
        assert_eq!(result.exit_code, Some(3));
        assert!(!result.timed_out);
    }

    /// What a platform containment hands back when it succeeds. A tree that
    /// constrains nothing is enough here: these tests are about the caller's
    /// handling, while the two shell tests above go through the platform's
    /// real containment.
    fn conforming_tree(_child: &Child) -> Tree {
        #[cfg(windows)]
        {
            Tree { job: None }
        }
        #[cfg(not(windows))]
        {
            Tree::unconstrained()
        }
    }
}
