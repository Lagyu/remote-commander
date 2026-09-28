mod blocking;
mod files;
mod processes;
mod screenshots;
mod transfers;

#[cfg(not(unix))]
compile_error!(
    "The native agent currently supports macOS and Linux; Windows process containment is not implemented."
);

use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use futures_util::{FutureExt, SinkExt, StreamExt, future::BoxFuture, stream::FuturesUnordered};
use rdc_protocol::{AgentResponse, Command, MAX_FRAME_BYTES, READ_LIMIT, ToolResult, WRITE_LIMIT};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashSet, VecDeque},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{Message, client::IntoClientRequest, protocol::WebSocketConfig},
};
use url::Url;

#[derive(Parser)]
#[command(
    version,
    about = "Pair an authorized computer with your self-hosted Remote Commander MCP service."
)]
struct Cli {
    #[command(subcommand)]
    command: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Display a short pairing code and save the approved device credential.
    Pair {
        #[arg(long)]
        server: String,
        #[arg(long)]
        name: String,
        #[arg(long, default_value_os_t = default_config())]
        config: PathBuf,
        #[arg(long)]
        no_browser: bool,
        #[arg(long)]
        insecure_localhost: bool,
    },
    /// Connect using an outbound WebSocket. Ctrl+C stops access and child processes.
    Run {
        #[arg(long, default_value_os_t = default_config())]
        config: PathBuf,
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        allow_write: bool,
        /// Grants shell commands the OS user's full authority; root is NOT a shell sandbox.
        #[arg(long)]
        allow_shell: bool,
        /// Allows reading the visible contents of a macOS display.
        #[arg(long)]
        allow_screenshot: bool,
        #[arg(long)]
        insecure_localhost: bool,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DeviceConfig {
    server: String,
    device_id: String,
    token: String,
}

fn default_config() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config/remote-commander/device.json")
}

fn server_url(value: &str, insecure: bool) -> Result<Url> {
    let url = Url::parse(value)?;
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path() == "/",
        "server must be an origin without credentials, path, query, or fragment"
    );
    let local = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    ensure!(
        url.scheme() == "https" || (insecure && local && url.scheme() == "http"),
        "HTTPS is required (loopback HTTP needs --insecure-localhost)"
    );
    Ok(url)
}

fn save_config(path: &Path, config: &DeviceConfig) -> Result<()> {
    let parent = path.parent().context("config needs a parent directory")?;
    if !parent.as_os_str().is_empty() && !parent.exists() {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .context("will not overwrite an existing credential file")?;
    file.write_all(&serde_json::to_vec_pretty(config)?)?;
    file.sync_all()?;
    Ok(())
}

fn load_config(path: &Path) -> Result<DeviceConfig> {
    let metadata = std::fs::symlink_metadata(path).context("pair this device first")?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "credential must be a regular file, not a symlink"
    );
    ensure!(metadata.len() <= 16_384, "credential file is too large");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            metadata.mode() & 0o077 == 0 && metadata.uid() == unsafe { libc::geteuid() },
            "credential file must belong to you and have mode 0600"
        );
    }
    let config: DeviceConfig = serde_json::from_slice(&std::fs::read(path)?)?;
    ensure!(
        config.token.len() >= 32 && config.token.len() <= 128,
        "invalid device token"
    );
    ensure!(
        !config.device_id.is_empty()
            && config.device_id.len() <= 80
            && config
                .device_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
        "invalid device identifier"
    );
    Ok(config)
}

async fn pair(
    server: String,
    name: String,
    path: PathBuf,
    no_browser: bool,
    insecure: bool,
) -> Result<()> {
    ensure!(
        !path.try_exists()?,
        "credential file already exists; use a new --config path or revoke the old device first"
    );
    ensure!(
        !name.trim().is_empty() && name.len() <= 80,
        "name must contain 1–80 bytes"
    );
    let base = server_url(&server, insecure)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let response = client
        .post(base.join("pair/start")?)
        .json(&json!({"name":name}))
        .send()
        .await?
        .error_for_status()?;
    let started: Value = response.json().await?;
    let code = started["device_code"]
        .as_str()
        .context("missing device_code")?;
    let verification = started["verification_uri_complete"]
        .as_str()
        .context("missing verification URL")?;
    let verification_url = Url::parse(verification)?;
    ensure!(
        verification_url.origin() == base.origin(),
        "pairing verification URL must use the configured server origin"
    );
    eprintln!(
        "Pairing code: {}\nOpen {} and approve only if the displayed code matches.",
        started["user_code"].as_str().unwrap_or(""),
        verification
    );
    if !no_browser {
        let browser = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let _ = std::process::Command::new(browser)
            .arg(verification)
            .status();
    }
    let deadline = Instant::now()
        + Duration::from_secs(started["expires_in"].as_u64().unwrap_or(600).min(600));
    let interval = started["interval"].as_u64().unwrap_or(2).clamp(2, 10);
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        let response = client
            .post(base.join("pair/token")?)
            .json(&json!({"device_code":code}))
            .send()
            .await?;
        let status = response.status();
        let body: Value = response.json().await?;
        if status.is_success() {
            let config = DeviceConfig {
                server: base.origin().ascii_serialization(),
                device_id: body["device_id"]
                    .as_str()
                    .context("missing device_id")?
                    .into(),
                token: body["token"]
                    .as_str()
                    .context("missing device token")?
                    .into(),
            };
            save_config(&path, &config)?;
            eprintln!(
                "Device paired. Private credential saved to {}.",
                path.display()
            );
            return Ok(());
        }
        match body["error"].as_str() {
            Some("authorization_pending" | "slow_down") => {}
            Some("expired_token" | "access_denied") => bail!("pairing expired or was denied"),
            _ => bail!("pairing request failed (HTTP {})", status.as_u16()),
        }
    }
    bail!("pairing expired; start a new pairing request")
}

struct Tools {
    files: Arc<files::FileTools>,
    processes: processes::ProcessTools,
    screenshots: Arc<screenshots::ScreenshotTools>,
    blocking: blocking::BlockingOperations,
}

struct Completion {
    id: String,
    name: String,
    result: ToolResult,
    started: Instant,
}

impl Tools {
    async fn execute(&self, command: &Command) -> ToolResult {
        if !command.arguments.is_object() {
            return ToolResult::error("invalid_arguments", "arguments must be an object");
        }
        if command.name == "get_screenshot" {
            let screenshots = self.screenshots.clone();
            let arguments = command.arguments.clone();
            return match self
                .blocking
                .run(move || screenshots.capture(arguments))
                .await
            {
                Ok(screenshot) => {
                    ToolResult::image(screenshot.data, "image/jpeg", screenshot.metadata)
                }
                Err(error) => ToolResult::error("operation_failed", error),
            };
        }
        let result = match command.name.as_str() {
            "get_config" => Ok(
                json!({"root":self.files.root,"allow_write":self.files.allow_write,"allow_shell":self.processes.enabled,"allow_screenshot":self.screenshots.enabled,"read_limit":READ_LIMIT,"write_limit":WRITE_LIMIT,"max_processes":8,"max_process_lifetime_ms":300000,"shell_is_sandboxed":false,"max_transfer_bytes":rdc_protocol::MAX_TRANSFER_BYTES,"transfer_chunk_bytes":rdc_protocol::TRANSFER_CHUNK_BYTES,"transfer_ttl_seconds":rdc_protocol::TRANSFER_TTL_SECONDS}),
            ),
            "shutdown_device" => Ok(json!({"stopping":true})),
            "ping_device" => Ok(json!({"ok":true,"platform":std::env::consts::OS})),
            name if rdc_protocol::scope_for(name) == Some(rdc_protocol::SCOPES[2]) => {
                self.processes
                    .execute(name, command.arguments.clone())
                    .await
            }
            _ => {
                let fs = self.files.clone();
                let name = command.name.clone();
                let arguments = command.arguments.clone();
                self.blocking
                    .run(move || fs.execute(&name, arguments))
                    .await
            }
        };
        match result {
            Ok(value) => {
                let mut result = ToolResult::ok(value);
                // Internal binary replies must not duplicate base64 into text content.
                if command.name == "transfer_download_chunk"
                    || command.name == "download_file_chunk"
                {
                    result.content.clear();
                }
                result
            }
            Err(error) => ToolResult::error("operation_failed", error),
        }
    }
}

async fn run_agent(config: &DeviceConfig, tools: &Tools, insecure: bool) -> Result<()> {
    let mut endpoint =
        server_url(&config.server, insecure)?.join(&format!("agent/{}", config.device_id))?;
    endpoint
        .set_scheme(if endpoint.scheme() == "https" {
            "wss"
        } else {
            "ws"
        })
        .map_err(|_| anyhow::anyhow!("invalid WebSocket URL"))?;
    let mut delay = 1u64;
    let mut cache: VecDeque<(String, ToolResult)> = VecDeque::new();
    let mut commands: FuturesUnordered<BoxFuture<'_, Completion>> = FuturesUnordered::new();
    let mut in_flight = HashSet::new();
    loop {
        let mut request = endpoint.as_str().into_client_request()?;
        request
            .headers_mut()
            .insert("Authorization", format!("Bearer {}", config.token).parse()?);
        let socket_config = WebSocketConfig::default()
            .max_message_size(Some(MAX_FRAME_BYTES))
            .max_frame_size(Some(MAX_FRAME_BYTES));
        let connection = tokio::time::timeout(
            Duration::from_secs(15),
            connect_async_with_config(request, Some(socket_config), false),
        )
        .await;
        let mut socket = match connection {
            Ok(Ok((socket, _))) => socket,
            Ok(Err(tokio_tungstenite::tungstenite::Error::Http(response)))
                if matches!(response.status().as_u16(), 401 | 403 | 404) =>
            {
                bail!("device access was rejected or revoked; pair again")
            }
            _ => {
                eprintln!("Connection unavailable. Reconnecting in {delay} seconds.");
                tokio::time::sleep(Duration::from_secs(delay)).await;
                delay = (delay * 2).min(30);
                continue;
            }
        };
        eprintln!(
            "Connected device {}. Root: {}. Writes: {}. Shell: {}. Screenshots: {}. Ctrl+C stops access.",
            config.device_id,
            tools.files.root.display(),
            tools.files.allow_write,
            tools.processes.enabled,
            tools.screenshots.enabled
        );
        let connected_at = Instant::now();
        let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
        let mut last_message = Instant::now();
        loop {
            tokio::select! {
                Some(Completion { id, name, result, started }) = commands.next(), if !commands.is_empty() => {
                    in_flight.remove(&id);
                    eprintln!("Tool {name} finished: error={}, elapsed_ms={}", result.is_error, started.elapsed().as_millis());
                    cache.push_back((id.clone(), result.clone()));
                    while cache.len() > 16 { cache.pop_front(); }
                    if socket.send(Message::Text(encode_response(id, result)?.into())).await.is_err() { break; }
                },
                _ = heartbeat.tick() => {
                    if last_message.elapsed() > Duration::from_secs(60) { break; }
                    if socket.send(Message::Ping(Vec::new().into())).await.is_err() { break; }
                },
                incoming = socket.next() => {
                    last_message = Instant::now();
                    match incoming {
                        Some(Ok(Message::Text(text))) => {
                            let command: Command = match serde_json::from_str(&text) {
                                Ok(command) => command,
                                Err(_) => { let _ = socket.close(None).await; break; }
                            };
                            if command.id.len() > 128 { let _ = socket.close(None).await; break; }
                            // Do not await tool work in the socket reader: a macOS
                            // consent prompt must not stop heartbeats or other tools.
                            if let Some((_, result)) = cache.iter().find(|(id,_)| id == &command.id) {
                                if socket.send(Message::Text(encode_response(command.id, result.clone())?.into())).await.is_err() { break; }
                                continue;
                            }
                            if in_flight.contains(&command.id) { continue; }
                            if matches!(command.name.as_str(), "ping_device" | "get_config" | "shutdown_device") {
                                let result = tools.execute(&command).await;
                                if socket.send(Message::Text(encode_response(command.id, result)?.into())).await.is_err() { break; }
                                if command.name == "shutdown_device" {
                                    let _ = socket.close(None).await;
                                    return Ok(());
                                }
                                continue;
                            }
                            if commands.len() >= 8 {
                                let result = ToolResult::error("busy", "At most eight tool calls may run on the agent. No operation was started.");
                                if socket.send(Message::Text(encode_response(command.id, result)?.into())).await.is_err() { break; }
                                continue;
                            }
                            // Log only catalog names, never arguments, paths or output.
                            if rdc_protocol::scope_for(&command.name).is_none()
                                && command.name != "download_file_chunk"
                                && !transfers::internal_tool(&command.name)
                            {
                                let result = ToolResult::error("unknown_tool", "Unknown agent tool");
                                if socket.send(Message::Text(encode_response(command.id, result)?.into())).await.is_err() { break; }
                                continue;
                            }
                            eprintln!("Tool {} started", command.name);
                            in_flight.insert(command.id.clone());
                            commands.push(async move {
                                let started = Instant::now();
                                let result = tools.execute(&command).await;
                                Completion { id: command.id, name: command.name, result, started }
                            }.boxed());
                        },
                        Some(Ok(Message::Ping(data))) => { if socket.send(Message::Pong(data)).await.is_err() { break; } },
                        Some(Ok(Message::Pong(_))) => {},
                        Some(Ok(Message::Close(frame))) => {
                            if frame.is_some_and(|f| matches!(u16::from(f.code), 4001 | 4003)) { bail!("device revoked or superseded; stopping"); }
                            break;
                        },
                        Some(Err(_)) | None => break,
                        _ => {},
                    }
                }
            }
        }
        if connected_at.elapsed() > Duration::from_secs(30) {
            delay = 1;
        }
        eprintln!("Connection lost. In-flight commands are not retried.");
        tokio::time::sleep(Duration::from_secs(delay)).await;
        delay = (delay * 2).min(30);
    }
}

fn encode_response(id: String, result: ToolResult) -> Result<String> {
    let mut response = AgentResponse { id, result };
    let encoded = serde_json::to_string(&response)?;
    if encoded.len() <= MAX_FRAME_BYTES {
        return Ok(encoded);
    }
    response.result = ToolResult::error(
        "output_limit",
        "result exceeds the transport limit; request a smaller page",
    );
    Ok(serde_json::to_string(&response)?)
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("signal handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

fn main() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(async_main());
    // spawn_blocking cannot cancel a syscall waiting for OS consent. Process
    // sessions are cleaned up in async_main; do not hang agent shutdown on it.
    runtime.shutdown_timeout(Duration::from_secs(2));
    result
}

async fn async_main() -> Result<()> {
    match Cli::parse().command {
        Action::Pair {
            server,
            name,
            config,
            no_browser,
            insecure_localhost,
        } => {
            tokio::select! {
                result = pair(server, name, config, no_browser, insecure_localhost) => result,
                _ = shutdown_signal() => Ok(()),
            }
        }
        Action::Run {
            config,
            root,
            allow_write,
            allow_shell,
            allow_screenshot,
            insecure_localhost,
        } => {
            let config = load_config(&config)?;
            let files = Arc::new(files::FileTools::new(&root, allow_write)?);
            let maintenance_files = Arc::downgrade(&files);
            let maintenance = tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(60));
                loop {
                    interval.tick().await;
                    let Some(files) = maintenance_files.upgrade() else {
                        break;
                    };
                    let _ = tokio::task::spawn_blocking(move || files.transfers.cleanup()).await;
                }
            });
            let tools = Tools {
                processes: processes::ProcessTools::new(files.root.clone(), allow_shell)?,
                screenshots: Arc::new(screenshots::ScreenshotTools::new(allow_screenshot)),
                files,
                blocking: blocking::BlockingOperations::default(),
            };
            let result = tokio::select! {
                result = run_agent(&config, &tools, insecure_localhost) => result,
                _ = shutdown_signal() => Ok(()),
            };
            maintenance.abort();
            tools.processes.shutdown().await;
            eprintln!("Device stopped; child processes cleaned up.");
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn server_validation_prevents_plaintext_and_credential_urls() {
        assert!(server_url("https://commander.example.com", false).is_ok());
        assert!(server_url("http://example.com", true).is_err());
        assert!(server_url("http://127.0.0.1:8787", false).is_err());
        assert!(server_url("http://127.0.0.1:8787", true).is_ok());
        assert!(server_url("https://user:password@example.com", false).is_err());
        assert!(server_url("https://example.com/?token=secret", false).is_err());
    }
}
