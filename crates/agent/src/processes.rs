use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{ChildStdin, Command},
    sync::{Mutex, watch},
    task::JoinHandle,
};

const OUTPUT_CAP: usize = 64 * 1024;

#[derive(Default)]
struct Output {
    bytes: VecDeque<u8>,
    base: u64,
    running: bool,
    exit_code: Option<i32>,
    timed_out: bool,
}

impl Output {
    fn append(&mut self, bytes: &[u8]) {
        self.bytes.extend(bytes);
        let excess = self.bytes.len().saturating_sub(OUTPUT_CAP);
        self.bytes.drain(..excess);
        self.base += excess as u64;
    }
    fn snapshot(&self, id: &str, pid: u32, cursor: u64) -> Value {
        let start = cursor
            .saturating_sub(self.base)
            .min(self.bytes.len() as u64) as usize;
        let data: Vec<u8> = self
            .bytes
            .iter()
            .skip(start)
            .take(rdc_protocol::READ_LIMIT)
            .copied()
            .collect();
        json!({"session_id":id,"pid":pid,"output":String::from_utf8_lossy(&data),"next_cursor":self.base+start as u64+data.len() as u64,"output_start":self.base,"truncated":cursor<self.base,"running":self.running,"exit_code":self.exit_code,"timed_out":self.timed_out})
    }
}

struct Session {
    pid: u32,
    started: Instant,
    output: Arc<StdMutex<Output>>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    cancel: watch::Sender<bool>,
    supervisor: JoinHandle<()>,
}

pub struct ProcessTools {
    root: PathBuf,
    pub enabled: bool,
    sessions: Mutex<BTreeMap<String, Session>>,
    blocking: crate::blocking::BlockingOperations,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    command: String,
    #[serde(default = "cwd")]
    cwd: String,
    #[serde(default = "timeout")]
    timeout_ms: u64,
}
fn cwd() -> String {
    ".".into()
}
fn timeout() -> u64 {
    60_000
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadOutput {
    session_id: String,
    #[serde(default)]
    cursor: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    session_id: String,
    input: String,
    #[serde(default)]
    close_stdin: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Id {
    session_id: String,
}

async fn drain(mut stream: impl AsyncRead + Unpin, output: Arc<StdMutex<Output>>) {
    let mut buffer = [0u8; 8192];
    loop {
        match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(size) => output.lock().unwrap().append(&buffer[..size]),
        }
    }
}

#[cfg(unix)]
fn kill_group(pid: u32) {
    // Every spawned child has its own process group. Negative PID targets only
    // that group, including children which inherited the shell's pipes.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}

impl ProcessTools {
    pub fn new(root: PathBuf, enabled: bool) -> Result<Self> {
        let root = root.canonicalize().context("process root must exist")?;
        ensure!(root.is_dir(), "process root must be a directory");
        Ok(Self {
            root,
            enabled,
            sessions: Mutex::new(BTreeMap::new()),
            blocking: crate::blocking::BlockingOperations::default(),
        })
    }

    async fn start(&self, args: Start) -> Result<Value> {
        ensure!(
            !args.command.is_empty() && args.command.len() <= 8192,
            "command must contain 1–8192 bytes"
        );
        ensure!(
            (100..=300_000).contains(&args.timeout_ms),
            "timeout_ms must be 100–300000"
        );
        let directory = self.root.join(crate::files::relative(&args.cwd)?);
        let root = self.root.clone();
        let directory = self
            .blocking
            .run(move || {
                let directory = directory.canonicalize()?;
                ensure!(
                    directory.starts_with(&root) && directory.is_dir(),
                    "cwd must be within root"
                );
                Ok(directory)
            })
            .await?;
        let mut sessions = self.sessions.lock().await;
        ensure!(
            sessions
                .values()
                .filter(|s| s.output.lock().unwrap().running)
                .count()
                < 8,
            "at most 8 processes may run simultaneously"
        );
        sessions.retain(|_, s| {
            s.output.lock().unwrap().running || s.started.elapsed() < Duration::from_secs(3600)
        });
        if sessions.len() >= 32 {
            let oldest = sessions
                .iter()
                .filter(|(_, s)| !s.output.lock().unwrap().running)
                .max_by_key(|(_, s)| s.started.elapsed())
                .map(|(id, _)| id.clone());
            if let Some(id) = oldest {
                sessions.remove(&id);
            }
        }
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(args.command)
            .current_dir(directory)
            .env_clear()
            .env(
                "PATH",
                std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
            )
            .env("LANG", "en_US.UTF-8")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(home) = std::env::var_os("HOME") {
            command.env("HOME", home);
        }
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn()?;
        let pid = child.id().context("child has no PID")?;
        let stdin = Arc::new(Mutex::new(child.stdin.take()));
        let output = Arc::new(StdMutex::new(Output {
            running: true,
            ..Output::default()
        }));
        let mut stdout = tokio::spawn(drain(child.stdout.take().unwrap(), output.clone()));
        let mut stderr = tokio::spawn(drain(child.stderr.take().unwrap(), output.clone()));
        let (cancel, mut cancelled) = watch::channel(false);
        let state = output.clone();
        let supervisor = tokio::spawn(async move {
            let (status, timed_out) = tokio::select! {
                status = child.wait() => (status.ok(), false),
                _ = cancelled.changed() => {
                    #[cfg(unix)] kill_group(pid);
                    let _ = child.kill().await;
                    (child.wait().await.ok(), false)
                },
                _ = tokio::time::sleep(Duration::from_millis(args.timeout_ms)) => {
                    #[cfg(unix)] kill_group(pid);
                    let _ = child.kill().await;
                    (child.wait().await.ok(), true)
                }
            };
            #[cfg(unix)]
            kill_group(pid);
            for reader in [&mut stdout, &mut stderr] {
                if tokio::time::timeout(Duration::from_secs(1), &mut *reader)
                    .await
                    .is_err()
                {
                    reader.abort();
                }
            }
            let mut state = state.lock().unwrap();
            state.running = false;
            state.exit_code = status.and_then(|s| s.code());
            state.timed_out = timed_out;
        });
        let id = uuid::Uuid::new_v4().to_string();
        let result = output.lock().unwrap().snapshot(&id, pid, 0);
        sessions.insert(
            id,
            Session {
                pid,
                started: Instant::now(),
                output,
                stdin,
                cancel,
                supervisor,
            },
        );
        Ok(result)
    }

    pub async fn execute(&self, name: &str, arguments: Value) -> Result<Value> {
        ensure!(
            self.enabled,
            "process execution is disabled; restart the agent with --allow-shell to enable it"
        );
        match name {
            "start_process" => self.start(serde_json::from_value(arguments)?).await,
            "read_process_output" => {
                let args: ReadOutput = serde_json::from_value(arguments)?;
                let sessions = self.sessions.lock().await;
                let session = sessions
                    .get(&args.session_id)
                    .context("session not found")?;
                Ok(session.output.lock().unwrap().snapshot(
                    &args.session_id,
                    session.pid,
                    args.cursor,
                ))
            }
            "interact_with_process" => {
                let args: Input = serde_json::from_value(arguments)?;
                ensure!(args.input.len() <= 8192, "input exceeds 8 KiB");
                let stdin = {
                    let sessions = self.sessions.lock().await;
                    let session = sessions
                        .get(&args.session_id)
                        .context("session not found")?;
                    ensure!(
                        session.output.lock().unwrap().running,
                        "process has already exited"
                    );
                    session.stdin.clone()
                };
                let mut handle = stdin.lock().await;
                let writer = handle.as_mut().context("stdin is closed")?;
                tokio::time::timeout(
                    Duration::from_secs(2),
                    writer.write_all(args.input.as_bytes()),
                )
                .await
                .context(
                    "process did not consume stdin; input may have been partially written",
                )??;
                if args.close_stdin {
                    handle.take();
                }
                Ok(
                    json!({"session_id":args.session_id,"accepted_bytes":args.input.len(),"stdin_closed":args.close_stdin}),
                )
            }
            "list_sessions" => {
                let sessions = self.sessions.lock().await;
                Ok(json!({"sessions":sessions.iter().map(|(id,s)| {
                    let output = s.output.lock().unwrap();
                    json!({"session_id":id,"pid":s.pid,"running":output.running,"exit_code":output.exit_code,"timed_out":output.timed_out})
                }).collect::<Vec<_>>()}))
            }
            "force_terminate" => {
                let args: Id = serde_json::from_value(arguments)?;
                let sessions = self.sessions.lock().await;
                let session = sessions
                    .get(&args.session_id)
                    .context("session not found")?;
                let _ = session.cancel.send(true);
                Ok(json!({"session_id":args.session_id,"termination_requested":true}))
            }
            _ => anyhow::bail!("unknown process tool"),
        }
    }

    pub async fn shutdown(&self) {
        let sessions = std::mem::take(&mut *self.sessions.lock().await);
        for session in sessions.values() {
            let _ = session.cancel.send(true);
        }
        for (_, session) in sessions {
            let _ = session.supervisor.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn stdin_exit_timeout_and_cleanup_are_real_process_operations() {
        let root = tempfile::tempdir().unwrap();
        let processes = ProcessTools::new(root.path().to_owned(), true).unwrap();
        let process = processes
            .execute(
                "start_process",
                json!({"command":"read value; printf 'got:%s' \"$value\"","timeout_ms":2000}),
            )
            .await
            .unwrap();
        let id = process["session_id"].as_str().unwrap();
        processes
            .execute(
                "interact_with_process",
                json!({"session_id":id,"input":"hello\n","close_stdin":true}),
            )
            .await
            .unwrap();
        for _ in 0..100 {
            let output = processes
                .execute("read_process_output", json!({"session_id":id}))
                .await
                .unwrap();
            if output["running"] == false {
                assert_eq!(output["output"], "got:hello");
                assert_eq!(output["exit_code"], 0);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let sleeper = processes
            .execute(
                "start_process",
                json!({"command":"sleep 30 & wait","timeout_ms":100}),
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(250)).await;
        let output = processes
            .execute(
                "read_process_output",
                json!({"session_id":sleeper["session_id"]}),
            )
            .await
            .unwrap();
        assert_eq!(output["running"], false);
        assert_eq!(output["timed_out"], true);
        let process = processes
            .execute("start_process", json!({"command":"sleep 30"}))
            .await
            .unwrap();
        let pid = process["pid"].as_u64().unwrap() as i32;
        processes.shutdown().await;
        #[cfg(unix)]
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    }
}
