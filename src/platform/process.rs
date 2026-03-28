use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use thiserror::Error;

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
    #[error("mock command runner exhausted its queued responses")]
    MockExhausted,
    #[error("mock command runner state became unavailable")]
    MockStateUnavailable,
}

pub trait CommandRunner: Send + Sync {
    fn run(&self, program: &str, args: &[&str], cwd: &Path) -> Result<ProcessOutput, ProcessError>;
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessInvocation {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
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
        });
        state
            .queued_results
            .pop_front()
            .ok_or(ProcessError::MockExhausted)?
    }
}

#[cfg(test)]
mod tests {
    use super::{CommandRunner, MockCommandRunner, ProcessOutput};

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
    }
}
