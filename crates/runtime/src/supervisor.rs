use crate::diagnostics::{BoundedDiagnostics, DiagnosticLine, DiagnosticStream};
use crate::health::{HealthProbe, validate_loopback};
use crate::restart::RestartPolicy;
use serde::Serialize;
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::net::IpAddr;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct WorkerSpec {
    pub id: String,
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
    pub working_directory: PathBuf,
    pub environment: Vec<(OsString, OsString)>,
    pub endpoint_ip: IpAddr,
}

impl WorkerSpec {
    fn validate(&self) -> Result<(), SupervisorError> {
        if self.id.trim().is_empty() {
            return Err(SupervisorError::InvalidSpec(
                "worker id cannot be empty".to_owned(),
            ));
        }
        if !self.executable.is_absolute() || !self.executable.is_file() {
            return Err(SupervisorError::InvalidSpec(format!(
                "worker executable must be an existing absolute file: {}",
                self.executable.display()
            )));
        }
        if !self.working_directory.is_absolute() || !self.working_directory.is_dir() {
            return Err(SupervisorError::InvalidSpec(format!(
                "worker directory must be an existing absolute directory: {}",
                self.working_directory.display()
            )));
        }
        validate_loopback(self.endpoint_ip).map_err(SupervisorError::InvalidSpec)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkerState {
    Stopped,
    Starting,
    Ready,
    Unhealthy,
    RestartPending,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerStatus {
    pub id: String,
    pub state: WorkerState,
    pub process_id: Option<u32>,
    pub restart_attempts: u32,
    pub last_exit_code: Option<i32>,
    pub diagnostics: Vec<DiagnosticLine>,
}

#[derive(Debug)]
pub enum SupervisorError {
    InvalidSpec(String),
    Io(std::io::Error),
    Platform(String),
}

impl std::fmt::Display for SupervisorError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSpec(message) | Self::Platform(message) => formatter.write_str(message),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SupervisorError {}

impl From<std::io::Error> for SupervisorError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

pub struct WorkerSupervisor {
    spec: WorkerSpec,
    health: Box<dyn HealthProbe>,
    restart_policy: RestartPolicy,
    child: Option<Child>,
    state: WorkerState,
    restart_attempts: u32,
    restart_at: Option<Instant>,
    started_at: Option<Instant>,
    ready_since: Option<Instant>,
    consecutive_health_failures: u32,
    last_exit_code: Option<i32>,
    diagnostics: BoundedDiagnostics,
    diagnostic_sender: Sender<(DiagnosticStream, String)>,
    diagnostic_receiver: Receiver<(DiagnosticStream, String)>,
    #[cfg(windows)]
    job: WindowsJob,
}

impl WorkerSupervisor {
    pub fn new(
        spec: WorkerSpec,
        health: Box<dyn HealthProbe>,
        restart_policy: RestartPolicy,
    ) -> Result<Self, SupervisorError> {
        spec.validate()?;
        let (diagnostic_sender, diagnostic_receiver) = mpsc::channel();
        Ok(Self {
            spec,
            health,
            restart_policy,
            child: None,
            state: WorkerState::Stopped,
            restart_attempts: 0,
            restart_at: None,
            started_at: None,
            ready_since: None,
            consecutive_health_failures: 0,
            last_exit_code: None,
            diagnostics: BoundedDiagnostics::new(256, 1024),
            diagnostic_sender,
            diagnostic_receiver,
            #[cfg(windows)]
            job: WindowsJob::new()?,
        })
    }

    pub fn start(&mut self) -> Result<(), SupervisorError> {
        if self.child.is_some() {
            return Ok(());
        }
        let mut command = Command::new(&self.spec.executable);
        command
            .args(&self.spec.arguments)
            .current_dir(&self.spec.working_directory)
            .envs(self.spec.environment.iter().cloned())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let mut child = command.spawn()?;
        #[cfg(windows)]
        if let Err(error) = self.job.assign(&child) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        if let Some(stdout) = child.stdout.take() {
            spawn_reader(
                stdout,
                DiagnosticStream::Stdout,
                self.diagnostic_sender.clone(),
            );
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_reader(
                stderr,
                DiagnosticStream::Stderr,
                self.diagnostic_sender.clone(),
            );
        }
        self.child = Some(child);
        self.state = WorkerState::Starting;
        self.restart_at = None;
        self.started_at = Some(Instant::now());
        self.ready_since = None;
        self.consecutive_health_failures = 0;
        self.diagnostics
            .push(DiagnosticStream::Supervisor, "worker process started");
        Ok(())
    }

    pub fn poll(&mut self) -> Result<WorkerStatus, SupervisorError> {
        self.drain_diagnostics();
        if let Some(child) = self.child.as_mut()
            && let Some(exit) = child.try_wait()?
        {
            self.last_exit_code = exit.code();
            self.child = None;
            self.schedule_restart();
        }

        if self.child.is_some() {
            match self.health.is_ready() {
                Ok(true) => self.record_healthy_poll(),
                Ok(false) => self.record_unhealthy_poll("health endpoint reported not ready")?,
                Err(error) => {
                    self.record_unhealthy_poll(&format!("health probe failed: {error}"))?
                }
            }
        } else if self
            .restart_at
            .is_some_and(|restart_at| Instant::now() >= restart_at)
        {
            self.start()?;
        }
        Ok(self.status())
    }

    pub fn stop(&mut self) -> Result<(), SupervisorError> {
        self.restart_at = None;
        self.started_at = None;
        self.ready_since = None;
        self.consecutive_health_failures = 0;
        if let Some(mut child) = self.child.take() {
            #[cfg(windows)]
            self.job.terminate()?;
            #[cfg(not(windows))]
            child.kill()?;
            child.wait()?;
        }
        self.state = WorkerState::Stopped;
        self.diagnostics
            .push(DiagnosticStream::Supervisor, "worker process stopped");
        Ok(())
    }

    #[must_use]
    pub fn status(&self) -> WorkerStatus {
        WorkerStatus {
            id: self.spec.id.clone(),
            state: self.state.clone(),
            process_id: self.child.as_ref().map(Child::id),
            restart_attempts: self.restart_attempts,
            last_exit_code: self.last_exit_code,
            diagnostics: self.diagnostics.snapshot(),
        }
    }

    fn schedule_restart(&mut self) {
        self.ready_since = None;
        self.started_at = None;
        self.consecutive_health_failures = 0;
        self.restart_attempts += 1;
        if let Some(delay) = self.restart_policy.delay_for_attempt(self.restart_attempts) {
            self.restart_at = Some(Instant::now() + delay);
            self.state = WorkerState::RestartPending;
            self.diagnostics.push(
                DiagnosticStream::Supervisor,
                format!("worker exited; restart {} scheduled", self.restart_attempts),
            );
        } else {
            self.restart_at = None;
            self.state = WorkerState::Failed;
            self.diagnostics.push(
                DiagnosticStream::Supervisor,
                "worker exhausted its restart budget",
            );
        }
    }

    fn record_healthy_poll(&mut self) {
        self.started_at = None;
        self.consecutive_health_failures = 0;
        let ready_since = self.ready_since.get_or_insert_with(Instant::now);
        if ready_since.elapsed() >= self.restart_policy.healthy_reset_after {
            self.restart_attempts = 0;
        }
        self.state = WorkerState::Ready;
    }

    fn record_unhealthy_poll(&mut self, reason: &str) -> Result<(), SupervisorError> {
        if self.state == WorkerState::Starting
            && self
                .started_at
                .is_some_and(|started| started.elapsed() < self.restart_policy.startup_timeout)
        {
            return Ok(());
        }
        self.ready_since = None;
        self.consecutive_health_failures = self.consecutive_health_failures.saturating_add(1);
        self.state = WorkerState::Unhealthy;
        self.diagnostics.push(
            DiagnosticStream::Supervisor,
            format!(
                "{reason}; failure {}/{}",
                self.consecutive_health_failures,
                self.restart_policy.maximum_consecutive_health_failures
            ),
        );
        if self.consecutive_health_failures
            >= self.restart_policy.maximum_consecutive_health_failures
        {
            self.terminate_current_process()?;
            self.schedule_restart();
        }
        Ok(())
    }

    fn terminate_current_process(&mut self) -> Result<(), SupervisorError> {
        if let Some(mut child) = self.child.take() {
            #[cfg(windows)]
            self.job.terminate()?;
            #[cfg(not(windows))]
            child.kill()?;
            child.wait()?;
        }
        Ok(())
    }

    fn drain_diagnostics(&mut self) {
        while let Ok((stream, line)) = self.diagnostic_receiver.try_recv() {
            self.diagnostics.push(stream, line);
        }
    }
}

impl Drop for WorkerSupervisor {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn spawn_reader(
    reader: impl std::io::Read + Send + 'static,
    stream: DiagnosticStream,
    sender: Sender<(DiagnosticStream, String)>,
) {
    std::thread::spawn(move || {
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            if sender.send((stream, line)).is_err() {
                break;
            }
        }
    });
}

#[cfg(windows)]
struct WindowsJob(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl WindowsJob {
    fn new() -> Result<Self, SupervisorError> {
        use std::mem::size_of;
        use std::ptr::null;
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        let handle = unsafe { CreateJobObjectW(null(), null()) };
        if handle.is_null() {
            return Err(SupervisorError::Io(std::io::Error::last_os_error()));
        }
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured == 0 {
            unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
            return Err(SupervisorError::Io(std::io::Error::last_os_error()));
        }
        Ok(Self(handle))
    }

    fn assign(&self, child: &Child) -> Result<(), SupervisorError> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
        let assigned = unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle()) };
        if assigned == 0 {
            return Err(SupervisorError::Platform(format!(
                "could not assign worker to Windows Job Object: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(())
    }

    fn terminate(&self) -> Result<(), SupervisorError> {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;
        let terminated = unsafe { TerminateJobObject(self.0, 1) };
        if terminated == 0 {
            return Err(SupervisorError::Platform(format!(
                "could not terminate Windows Job Object: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for WindowsJob {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

unsafe impl Send for WindowsJob {}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::fs;
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    struct AlwaysReady;

    impl HealthProbe for AlwaysReady {
        fn is_ready(&self) -> Result<bool, String> {
            Ok(true)
        }
    }

    #[test]
    fn stop_terminates_worker_descendants() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let pid_file = std::env::temp_dir().join(format!("fasttalk-worker-child-{suffix}.pid"));
        let escaped_pid_file = pid_file.display().to_string().replace('\'', "''");
        let script = format!(
            "$p=Start-Process -FilePath \"$env:SystemRoot\\System32\\ping.exe\" -ArgumentList @('-t','127.0.0.1') -WindowStyle Hidden -PassThru; Set-Content -LiteralPath '{escaped_pid_file}' -Value $p.Id; Wait-Process -Id $p.Id"
        );
        let powershell =
            PathBuf::from(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe");
        let working_directory = std::env::current_dir().unwrap();
        let spec = WorkerSpec {
            id: "job-object-test".to_owned(),
            executable: powershell,
            arguments: ["-NoProfile", "-NonInteractive", "-Command"]
                .into_iter()
                .map(OsString::from)
                .chain(std::iter::once(OsString::from(script)))
                .collect(),
            working_directory,
            environment: Vec::new(),
            endpoint_ip: "127.0.0.1".parse().unwrap(),
        };
        let mut supervisor =
            WorkerSupervisor::new(spec, Box::new(AlwaysReady), RestartPolicy::default()).unwrap();
        supervisor.start().unwrap();
        assert_eq!(supervisor.poll().unwrap().state, WorkerState::Ready);

        let deadline = Instant::now() + Duration::from_secs(5);
        while !pid_file.is_file() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        let descendant_pid: u32 = fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(process_exists(descendant_pid));

        supervisor.stop().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while process_exists(descendant_pid) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!process_exists(descendant_pid));
        let _ = fs::remove_file(pid_file);
    }

    fn process_exists(process_id: u32) -> bool {
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
        if handle.is_null() {
            return false;
        }
        unsafe { CloseHandle(handle) };
        true
    }
}
