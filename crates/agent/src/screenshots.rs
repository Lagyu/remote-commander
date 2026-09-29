#[cfg(target_os = "macos")]
use anyhow::{Context, anyhow};
use anyhow::{Result, bail, ensure};
#[cfg(target_os = "macos")]
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{Value, json};
#[cfg(target_os = "macos")]
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
#[cfg(target_os = "macos")]
use uuid::Uuid;

#[cfg(target_os = "macos")]
const MAX_SCREENSHOT_BYTES: usize = 350 * 1024;
const DEFAULT_MAX_DIMENSION: u32 = 1600;
const DEFAULT_QUALITY: u8 = 65;

#[derive(Clone)]
pub struct ScreenshotTools {
    pub enabled: bool,
}

#[derive(Debug)]
pub struct Screenshot {
    pub data: String,
    pub metadata: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScreenshotArgs {
    #[serde(default = "default_display")]
    display: u8,
    #[serde(default = "default_max_dimension")]
    max_dimension: u32,
    #[serde(default = "default_quality")]
    quality: u8,
    #[serde(default)]
    include_cursor: bool,
}

fn default_display() -> u8 {
    1
}

fn default_max_dimension() -> u32 {
    DEFAULT_MAX_DIMENSION
}

fn default_quality() -> u8 {
    DEFAULT_QUALITY
}

impl ScreenshotTools {
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    pub fn capture(&self, arguments: Value) -> Result<Screenshot> {
        ensure!(
            self.enabled,
            "screen capture is disabled; restart the agent without --no-screenshot to enable it"
        );
        let args: ScreenshotArgs = serde_json::from_value(arguments)?;
        ensure!(
            (1..=16).contains(&args.display),
            "display must be between 1 and 16"
        );
        ensure!(
            (320..=2560).contains(&args.max_dimension),
            "max_dimension must be between 320 and 2560"
        );
        ensure!(
            (30..=90).contains(&args.quality),
            "quality must be between 30 and 90"
        );

        #[cfg(target_os = "macos")]
        {
            capture_macos(args)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = args;
            bail!("get_screenshot is currently supported only on macOS")
        }
    }
}

#[cfg(target_os = "macos")]
fn capture_macos(args: ScreenshotArgs) -> Result<Screenshot> {
    let temp = TempScreenshot::new();
    let mut capture = Command::new("/usr/sbin/screencapture");
    capture
        .arg("-x")
        .arg("-r")
        .arg(format!("-D{}", args.display))
        .arg("-tjpg");
    if args.include_cursor {
        capture.arg("-C");
    }
    capture.arg(&temp.source);
    let output = capture
        .output()
        .context("could not start macOS screencapture")?;
    command_ok(
        "screencapture",
        &output,
        "macOS Screen Recording permission may be required",
    )?;
    ensure!(
        temp.source.is_file(),
        "screencapture completed without producing an image; macOS Screen Recording permission may be required"
    );

    let mut dimension = args.max_dimension;
    let mut quality = args.quality;
    for _ in 0..6 {
        let _ = fs::remove_file(&temp.output);
        let output = Command::new("/usr/bin/sips")
            .arg("-Z")
            .arg(dimension.to_string())
            .args(["-s", "format", "jpeg"])
            .args(["-s", "formatOptions"])
            .arg(quality.to_string())
            .arg(&temp.source)
            .arg("-o")
            .arg(&temp.output)
            .output()
            .context("could not start macOS image encoder")?;
        command_ok("sips", &output, "could not encode screenshot")?;
        let bytes = fs::read(&temp.output).context("could not read encoded screenshot")?;
        ensure!(!bytes.is_empty(), "encoded screenshot is empty");
        if bytes.len() <= MAX_SCREENSHOT_BYTES {
            let length = bytes.len();
            return Ok(Screenshot {
                data: STANDARD.encode(bytes),
                metadata: json!({
                    "display": args.display,
                    "bytes": length,
                    "mime_type": "image/jpeg",
                    "max_dimension": dimension,
                    "quality": quality,
                    "include_cursor": args.include_cursor
                }),
            });
        }

        let next_dimension = (dimension * 3 / 4).max(320);
        let next_quality = quality.saturating_sub(10).max(30);
        if next_dimension == dimension && next_quality == quality {
            break;
        }
        dimension = next_dimension;
        quality = next_quality;
    }

    bail!(
        "screenshot could not be reduced below the {} KiB transport budget",
        MAX_SCREENSHOT_BYTES / 1024
    )
}

#[cfg(target_os = "macos")]
fn command_ok(name: &str, output: &Output, fallback: &str) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail: String = stderr.trim().chars().take(512).collect();
    if detail.is_empty() {
        Err(anyhow!("{name} failed: {fallback}"))
    } else {
        Err(anyhow!("{name} failed: {detail}"))
    }
}

#[cfg(target_os = "macos")]
struct TempScreenshot {
    source: PathBuf,
    output: PathBuf,
}

#[cfg(target_os = "macos")]
impl TempScreenshot {
    fn new() -> Self {
        let id = Uuid::new_v4();
        let base = std::env::temp_dir();
        Self {
            source: base.join(format!("remote-commander-{id}-source.jpg")),
            output: base.join(format!("remote-commander-{id}-output.jpg")),
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for TempScreenshot {
    fn drop(&mut self) {
        remove_if_regular(&self.source);
        remove_if_regular(&self.output);
    }
}

#[cfg(target_os = "macos")]
fn remove_if_regular(path: &Path) {
    if path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.is_file())
    {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_respects_local_opt_out() {
        let tools = ScreenshotTools::new(false);
        let error = tools.capture(json!({})).unwrap_err();
        assert!(error.to_string().contains("--no-screenshot"));
    }

    #[test]
    fn capture_validates_arguments_before_touching_the_screen() {
        let tools = ScreenshotTools::new(true);
        assert!(
            tools
                .capture(json!({"display":0}))
                .unwrap_err()
                .to_string()
                .contains("display")
        );
        assert!(
            tools
                .capture(json!({"max_dimension":319}))
                .unwrap_err()
                .to_string()
                .contains("max_dimension")
        );
        assert!(
            tools
                .capture(json!({"quality":29}))
                .unwrap_err()
                .to_string()
                .contains("quality")
        );
    }
}
