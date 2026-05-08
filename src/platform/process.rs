use std::collections::VecDeque;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use thiserror::Error;

const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(50);
const TIMEOUT_CLEANUP_GRACE: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: i32,
}

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("failed to run `{program}` in `{cwd}`: {source}")]
    Io {
        program: String,
        cwd: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("`{program}` timed out after {timeout_secs}s in `{cwd}`")]
    Timeout {
        program: String,
        cwd: PathBuf,
        timeout_secs: u64,
    },
    #[error("mock command runner exhausted its queued responses")]
    MockExhausted,
    #[error("mock command runner state became unavailable")]
    MockStateUnavailable,
}

pub trait CommandRunner: Send + Sync {
    fn run(&self, program: &str, args: &[&str], cwd: &Path) -> Result<ProcessOutput, ProcessError>;

    fn run_with_timeout(
        &self,
        program: &str,
        args: &[&str],
        cwd: &Path,
        timeout: Duration,
    ) -> Result<ProcessOutput, ProcessError> {
        let _ = timeout;
        self.run(program, args, cwd)
    }
}

#[derive(Clone, Debug, Default)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, program: &str, args: &[&str], cwd: &Path) -> Result<ProcessOutput, ProcessError> {
        let output = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .output()
            .map_err(|source| ProcessError::Io {
                program: program.to_owned(),
                cwd: cwd.to_path_buf(),
                source,
            })?;

        Ok(ProcessOutput {
            stdout: output.stdout,
            stderr: output.stderr,
            exit_code: output.status.code().unwrap_or(-1),
        })
    }

    fn run_with_timeout(
        &self,
        program: &str,
        args: &[&str],
        cwd: &Path,
        timeout: Duration,
    ) -> Result<ProcessOutput, ProcessError> {
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_process_group(&mut command);

        let mut child = command.spawn().map_err(|source| ProcessError::Io {
            program: program.to_owned(),
            cwd: cwd.to_path_buf(),
            source,
        })?;
        let child_id = child.id();
        let stdout = child.stdout.take().ok_or_else(|| ProcessError::Io {
            program: program.to_owned(),
            cwd: cwd.to_path_buf(),
            source: io::Error::other("child stdout pipe was unavailable"),
        })?;
        let stderr = child.stderr.take().ok_or_else(|| ProcessError::Io {
            program: program.to_owned(),
            cwd: cwd.to_path_buf(),
            source: io::Error::other("child stderr pipe was unavailable"),
        })?;
        let stdout_rx = drain_pipe(stdout);
        let stderr_rx = drain_pipe(stderr);

        let started = Instant::now();
        loop {
            match child.try_wait().map_err(|source| ProcessError::Io {
                program: program.to_owned(),
                cwd: cwd.to_path_buf(),
                source,
            })? {
                Some(status) => {
                    let stdout = match collect_pipe(
                        &stdout_rx,
                        remaining_timeout(started, timeout),
                        program,
                        cwd,
                    )? {
                        PipeCollection::Collected(output) => output,
                        PipeCollection::DeadlineExpired => {
                            return timeout_after_cleanup(
                                &mut child, child_id, &stdout_rx, &stderr_rx, program, cwd, timeout,
                            );
                        }
                    };
                    let stderr = match collect_pipe(
                        &stderr_rx,
                        remaining_timeout(started, timeout),
                        program,
                        cwd,
                    )? {
                        PipeCollection::Collected(output) => output,
                        PipeCollection::DeadlineExpired => {
                            return timeout_after_cleanup(
                                &mut child, child_id, &stdout_rx, &stderr_rx, program, cwd, timeout,
                            );
                        }
                    };
                    return Ok(ProcessOutput {
                        stdout,
                        stderr,
                        exit_code: status.code().unwrap_or(-1),
                    });
                }
                None if started.elapsed() >= timeout => {
                    return timeout_after_cleanup(
                        &mut child, child_id, &stdout_rx, &stderr_rx, program, cwd, timeout,
                    );
                }
                None => std::thread::sleep(PROCESS_POLL_INTERVAL),
            }
        }
    }
}

fn drain_pipe<R>(mut reader: R) -> Receiver<io::Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut output = Vec::new();
        let result = reader.read_to_end(&mut output).map(|_| output);
        let _ = tx.send(result);
    });
    rx
}

enum PipeCollection {
    Collected(Vec<u8>),
    DeadlineExpired,
}

fn collect_pipe(
    rx: &Receiver<io::Result<Vec<u8>>>,
    timeout: Option<Duration>,
    program: &str,
    cwd: &Path,
) -> Result<PipeCollection, ProcessError> {
    let result = match timeout {
        Some(timeout) => match rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => return Ok(PipeCollection::DeadlineExpired),
            Err(source @ mpsc::RecvTimeoutError::Disconnected) => {
                return Err(ProcessError::Io {
                    program: program.to_owned(),
                    cwd: cwd.to_path_buf(),
                    source: io::Error::new(io::ErrorKind::BrokenPipe, source),
                });
            }
        },
        None => rx.recv().map_err(|source| ProcessError::Io {
            program: program.to_owned(),
            cwd: cwd.to_path_buf(),
            source: io::Error::new(io::ErrorKind::BrokenPipe, source),
        })?,
    };
    result
        .map(PipeCollection::Collected)
        .map_err(|source| ProcessError::Io {
            program: program.to_owned(),
            cwd: cwd.to_path_buf(),
            source,
        })
}

fn remaining_timeout(started: Instant, timeout: Duration) -> Option<Duration> {
    Some(timeout.saturating_sub(started.elapsed()))
}

fn timeout_after_cleanup(
    child: &mut std::process::Child,
    child_id: u32,
    stdout_rx: &Receiver<io::Result<Vec<u8>>>,
    stderr_rx: &Receiver<io::Result<Vec<u8>>>,
    program: &str,
    cwd: &Path,
    timeout: Duration,
) -> Result<ProcessOutput, ProcessError> {
    terminate_process_tree(child, child_id);
    wait_with_deadline(child, TIMEOUT_CLEANUP_GRACE);
    let _ = collect_pipe(stdout_rx, Some(TIMEOUT_CLEANUP_GRACE), program, cwd);
    let _ = collect_pipe(stderr_rx, Some(TIMEOUT_CLEANUP_GRACE), program, cwd);
    Err(ProcessError::Timeout {
        program: program.to_owned(),
        cwd: cwd.to_path_buf(),
        timeout_secs: display_timeout_secs(timeout),
    })
}

fn display_timeout_secs(timeout: Duration) -> u64 {
    timeout.as_secs() + u64::from(timeout.subsec_nanos() > 0)
}

fn wait_with_deadline(child: &mut std::process::Child, grace: Duration) {
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => std::thread::sleep(PROCESS_POLL_INTERVAL),
            Err(_) => return,
        }
    }
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    unsafe {
        command.pre_exec(|| {
            if setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

#[cfg(unix)]
fn terminate_process_tree(child: &mut std::process::Child, child_id: u32) {
    unsafe {
        let _ = kill(-(child_id as Pid), SIGKILL);
    }
    let _ = child.kill();
}

#[cfg(not(unix))]
fn terminate_process_tree(child: &mut std::process::Child, _child_id: u32) {
    let _ = child.kill();
}

#[cfg(unix)]
type Pid = i32;

#[cfg(unix)]
const SIGKILL: i32 = 9;

#[cfg(unix)]
unsafe extern "C" {
    fn setsid() -> Pid;
    fn kill(pid: Pid, sig: i32) -> i32;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessInvocation {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub timeout: Option<Duration>,
}

#[derive(Clone, Debug, Default)]
pub struct MockCommandRunner {
    state: Arc<Mutex<MockCommandRunnerState>>,
}

#[derive(Debug, Default)]
struct MockCommandRunnerState {
    queued_results: VecDeque<Result<ProcessOutput, ProcessError>>,
    invocations: Vec<ProcessInvocation>,
}

impl MockCommandRunner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_result(
        &self,
        result: Result<ProcessOutput, ProcessError>,
    ) -> Result<(), ProcessError> {
        let mut state = self.lock_state()?;
        state.queued_results.push_back(result);
        Ok(())
    }

    pub fn invocations(&self) -> Result<Vec<ProcessInvocation>, ProcessError> {
        let state = self.lock_state()?;
        Ok(state.invocations.clone())
    }

    fn lock_state(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, MockCommandRunnerState>, ProcessError> {
        self.state
            .lock()
            .map_err(|_| ProcessError::MockStateUnavailable)
    }
}

impl CommandRunner for MockCommandRunner {
    fn run(&self, program: &str, args: &[&str], cwd: &Path) -> Result<ProcessOutput, ProcessError> {
        let mut state = self.lock_state()?;
        state.invocations.push(ProcessInvocation {
            program: program.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            cwd: cwd.to_path_buf(),
            timeout: None,
        });
        state
            .queued_results
            .pop_front()
            .ok_or(ProcessError::MockExhausted)?
    }

    fn run_with_timeout(
        &self,
        program: &str,
        args: &[&str],
        cwd: &Path,
        timeout: Duration,
    ) -> Result<ProcessOutput, ProcessError> {
        let mut state = self.lock_state()?;
        state.invocations.push(ProcessInvocation {
            program: program.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            cwd: cwd.to_path_buf(),
            timeout: Some(timeout),
        });
        state
            .queued_results
            .pop_front()
            .ok_or(ProcessError::MockExhausted)?
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{
        CommandRunner, MockCommandRunner, ProcessError, ProcessOutput, SystemCommandRunner,
    };

    #[test]
    fn mock_command_runner_records_invocations() {
        let runner = MockCommandRunner::new();
        runner
            .push_result(Ok(ProcessOutput {
                stdout: b"ok".to_vec(),
                stderr: Vec::new(),
                exit_code: 0,
            }))
            .unwrap();

        let output = runner
            .run("codeql", &["database", "create"], "/workspace".as_ref())
            .unwrap();

        assert_eq!(output.stdout, b"ok".to_vec());
        let invocations = runner.invocations().unwrap();
        assert_eq!(invocations.len(), 1);
        assert_eq!(invocations[0].program, "codeql");
        assert_eq!(invocations[0].args, vec!["database", "create"]);
        assert_eq!(invocations[0].timeout, None);
    }

    #[test]
    fn mock_command_runner_records_timeout_invocations() {
        let runner = MockCommandRunner::new();
        runner
            .push_result(Ok(ProcessOutput {
                stdout: b"ok".to_vec(),
                stderr: Vec::new(),
                exit_code: 0,
            }))
            .unwrap();

        let output = runner
            .run_with_timeout(
                "codeql",
                &["query", "run"],
                "/workspace".as_ref(),
                Duration::from_secs(240),
            )
            .unwrap();

        assert_eq!(output.stdout, b"ok".to_vec());
        let invocations = runner.invocations().unwrap();
        assert_eq!(invocations.len(), 1);
        assert_eq!(invocations[0].program, "codeql");
        assert_eq!(invocations[0].args, vec!["query", "run"]);
        assert_eq!(invocations[0].timeout, Some(Duration::from_secs(240)));
    }

    #[cfg(unix)]
    #[test]
    fn system_command_runner_drains_large_stdout_and_stderr_while_process_runs() {
        let runner = SystemCommandRunner;

        let output = runner
            .run_with_timeout(
                "sh",
                &[
                    "-c",
                    "awk 'BEGIN { for (i = 0; i < 262144; i++) printf \"x\" }'; awk 'BEGIN { for (i = 0; i < 262144; i++) printf \"e\" }' >&2",
                ],
                ".".as_ref(),
                Duration::from_secs(5),
            )
            .unwrap();

        assert_eq!(output.exit_code, 0);
        assert_eq!(output.stdout.len(), 262_144);
        assert_eq!(output.stderr.len(), 262_144);
    }

    #[cfg(unix)]
    #[test]
    fn system_command_runner_timeout_does_not_wait_indefinitely_on_pipe_holding_descendant() {
        let runner = SystemCommandRunner;
        let started = Instant::now();

        let error = runner
            .run_with_timeout(
                "sh",
                &["-c", "(sleep 30) & echo started; sleep 30"],
                ".".as_ref(),
                Duration::from_millis(100),
            )
            .unwrap_err();

        assert!(matches!(error, ProcessError::Timeout { .. }));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "timeout cleanup should be bounded, elapsed {:?}",
            started.elapsed()
        );
    }

    #[cfg(unix)]
    #[test]
    fn system_command_runner_timeout_bounds_pipe_collection_after_direct_child_exits() {
        let runner = SystemCommandRunner;
        let started = Instant::now();

        let error = runner
            .run_with_timeout(
                "sh",
                &["-c", "(sleep 30) & echo started"],
                ".".as_ref(),
                Duration::from_millis(100),
            )
            .unwrap_err();

        assert!(matches!(error, ProcessError::Timeout { .. }));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "pipe collection timeout cleanup should be bounded, elapsed {:?}",
            started.elapsed()
        );
    }
}
