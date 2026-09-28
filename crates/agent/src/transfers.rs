//! Bounded binary transfers. File handles stay rooted; uploads publish only on commit.
use crate::files::relative;
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use cap_std::fs::{Dir, File, Metadata, OpenOptions};
use rdc_protocol::{MAX_TRANSFER_BYTES, TRANSFER_CHUNK_BYTES, TRANSFER_TTL_SECONDS};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    sync::Mutex,
    time::{Duration, Instant},
};

const MAX_SESSIONS: usize = 8;

pub fn internal_tool(name: &str) -> bool {
    matches!(
        name,
        "transfer_download_begin"
            | "transfer_download_chunk"
            | "transfer_upload_begin"
            | "transfer_upload_chunk"
            | "transfer_upload_complete"
            | "transfer_status"
            | "transfer_cancel"
    )
}

pub struct TransferTools {
    dir: Dir,
    allow_write: bool,
    sessions: Mutex<HashMap<String, Session>>,
}
struct Session {
    created: Instant,
    path: String,
    size: u64,
    body: Transfer,
}
enum Transfer {
    Download { file: File, metadata: Metadata },
    Upload(Upload),
}
struct Upload {
    parent: Dir,
    temporary: String,
    name: String,
    file: Option<File>,
    written: u64,
    hash: Sha256,
    expected_hash: Option<String>,
    overwrite: bool,
    complete: bool,
}
impl Drop for Upload {
    fn drop(&mut self) {
        // Only this session's randomly named, create_new file is removed.
        let _ = self.parent.remove_file(&self.temporary);
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DownloadBegin {
    path: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadBegin {
    path: String,
    size: u64,
    #[serde(default)]
    overwrite: bool,
    #[serde(default)]
    sha256: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Id {
    transfer_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DownloadChunk {
    transfer_id: String,
    offset: u64,
    length: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadChunk {
    transfer_id: String,
    offset: u64,
    data: String,
}

fn same_file_state(a: &Metadata, b: &Metadata) -> bool {
    a.len() == b.len() && a.modified().ok() == b.modified().ok()
}
fn status(id: &str, session: &Session) -> Value {
    match &session.body {
        Transfer::Download { .. } => {
            json!({"transfer_id":id,"path":session.path,"size":session.size,"direction":"download"})
        }
        Transfer::Upload(upload) => {
            json!({"transfer_id":id,"path":session.path,"size":session.size,"direction":"upload","bytes_received":upload.written,"complete":upload.complete,"sha256":if upload.complete { Some(format!("{:x}",upload.hash.clone().finalize())) } else { None }})
        }
    }
}

impl TransferTools {
    pub fn new(dir: Dir, allow_write: bool) -> Self {
        Self {
            dir,
            allow_write,
            sessions: Mutex::new(HashMap::new()),
        }
    }
    pub fn cleanup(&self) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.retain(|_, s| s.created.elapsed() < Duration::from_secs(TRANSFER_TTL_SECONDS));
        }
    }
    pub fn execute(&self, name: &str, arguments: Value) -> Result<Value> {
        self.cleanup();
        if name.starts_with("transfer_upload_") {
            ensure!(
                self.allow_write,
                "file writes are disabled; restart the agent with --allow-write to enable them"
            );
        }
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("transfer state unavailable"))?;
        match name {
            "transfer_download_begin" => {
                let args: DownloadBegin = serde_json::from_value(arguments)?;
                ensure!(
                    sessions.len() < MAX_SESSIONS,
                    "at most eight file transfer sessions may be retained; cancel an old transfer first"
                );
                let path = relative(&args.path)?;
                let entry = self.dir.symlink_metadata(path)?;
                ensure!(
                    entry.is_file() && !entry.file_type().is_symlink(),
                    "expected a regular file, not a symlink"
                );
                let mut options = OpenOptions::new();
                options.read(true);
                #[cfg(unix)]
                {
                    use cap_std::fs::OpenOptionsExt;
                    options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
                }
                let file = self
                    .dir
                    .open_with(path, &options)
                    .context("cannot open file within root")?;
                let metadata = file.metadata()?;
                ensure!(metadata.is_file(), "expected a regular file");
                ensure!(
                    metadata.len() <= MAX_TRANSFER_BYTES,
                    "file exceeds the 1 GiB (1073741824 byte) transfer limit"
                );
                let id = uuid::Uuid::new_v4().to_string();
                let session = Session {
                    created: Instant::now(),
                    path: args.path,
                    size: metadata.len(),
                    body: Transfer::Download { file, metadata },
                };
                let result = status(&id, &session);
                sessions.insert(id, session);
                Ok(result)
            }
            "transfer_upload_begin" => {
                let args: UploadBegin = serde_json::from_value(arguments)?;
                ensure!(
                    sessions.len() < MAX_SESSIONS,
                    "at most eight file transfer sessions may be retained; cancel an old transfer first"
                );
                ensure!(
                    args.size <= MAX_TRANSFER_BYTES,
                    "file exceeds the 1 GiB (1073741824 byte) transfer limit"
                );
                if let Some(hash) = &args.sha256 {
                    ensure!(
                        hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
                        "sha256 must contain exactly 64 hexadecimal characters"
                    );
                }
                let path = relative(&args.path)?;
                let name = path
                    .file_name()
                    .and_then(|v| v.to_str())
                    .context("expected a UTF-8 file name")?
                    .to_owned();
                let parent = path
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
                let parent = self
                    .dir
                    .open_dir(parent)
                    .context("destination parent must exist within root")?;
                match parent.symlink_metadata(&name) {
                    Ok(meta) => {
                        ensure!(
                            args.overwrite,
                            "destination already exists; explicitly set overwrite to replace it"
                        );
                        ensure!(
                            meta.is_file() && !meta.file_type().is_symlink(),
                            "destination must be a regular file, not a symlink"
                        );
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
                let id = uuid::Uuid::new_v4().to_string();
                let temporary = format!(".rdc-upload-{id}.tmp");
                let mut options = OpenOptions::new();
                options.read(true).write(true).create_new(true);
                #[cfg(unix)]
                {
                    use cap_std::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let file = parent.open_with(&temporary, &options)?;
                let upload = Upload {
                    parent,
                    temporary,
                    name,
                    file: Some(file),
                    written: 0,
                    hash: Sha256::new(),
                    expected_hash: args.sha256.map(|v| v.to_ascii_lowercase()),
                    overwrite: args.overwrite,
                    complete: false,
                };
                let session = Session {
                    created: Instant::now(),
                    path: args.path,
                    size: args.size,
                    body: Transfer::Upload(upload),
                };
                let result = status(&id, &session);
                sessions.insert(id, session);
                Ok(result)
            }
            "transfer_download_chunk" => {
                let args: DownloadChunk = serde_json::from_value(arguments)?;
                ensure!(
                    args.length > 0 && args.length <= TRANSFER_CHUNK_BYTES,
                    "invalid download chunk size"
                );
                let session = sessions
                    .get_mut(&args.transfer_id)
                    .context("transfer expired or not found")?;
                ensure!(
                    args.offset <= session.size,
                    "download offset exceeds file size"
                );
                let Transfer::Download { file, metadata } = &mut session.body else {
                    bail!("not a download transfer")
                };
                ensure!(
                    same_file_state(metadata, &file.metadata()?),
                    "source file changed; create a new download link"
                );
                let length = (session.size - args.offset).min(args.length as u64) as usize;
                let mut bytes = vec![0; length];
                file.seek(SeekFrom::Start(args.offset))?;
                file.read_exact(&mut bytes)?;
                ensure!(
                    same_file_state(metadata, &file.metadata()?),
                    "source file changed during download"
                );
                Ok(
                    json!({"offset":args.offset,"size":session.size,"bytes":length,"data":STANDARD.encode(bytes)}),
                )
            }
            "transfer_upload_chunk" => {
                let args: UploadChunk = serde_json::from_value(arguments)?;
                ensure!(
                    args.data.len() <= TRANSFER_CHUNK_BYTES.div_ceil(3) * 4,
                    "upload chunk exceeds 1 MiB"
                );
                let bytes = STANDARD
                    .decode(&args.data)
                    .context("invalid base64 chunk")?;
                ensure!(
                    !bytes.is_empty() && bytes.len() <= TRANSFER_CHUNK_BYTES,
                    "upload chunk must contain 1 byte to 1 MiB"
                );
                let session = sessions
                    .get_mut(&args.transfer_id)
                    .context("transfer expired or not found")?;
                let end = args
                    .offset
                    .checked_add(bytes.len() as u64)
                    .context("invalid offset")?;
                ensure!(
                    end <= session.size && end <= MAX_TRANSFER_BYTES,
                    "chunk exceeds declared file size"
                );
                let Transfer::Upload(upload) = &mut session.body else {
                    bail!("not an upload transfer")
                };
                ensure!(!upload.complete, "upload already completed");
                let file = upload.file.as_mut().context("upload file is closed")?;
                if args.offset < upload.written {
                    // Retries are safe only when the already-acknowledged bytes match.
                    ensure!(end <= upload.written, "retry overlaps unacknowledged data");
                    let mut previous = vec![0; bytes.len()];
                    file.seek(SeekFrom::Start(args.offset))?;
                    file.read_exact(&mut previous)?;
                    ensure!(
                        previous == bytes,
                        "retry data does not match the stored chunk"
                    );
                } else {
                    ensure!(
                        args.offset == upload.written,
                        "out-of-order chunk; resume at bytes_received"
                    );
                    file.seek(SeekFrom::Start(args.offset))?;
                    file.write_all(&bytes)?;
                    upload.hash.update(&bytes);
                    upload.written = end;
                }
                Ok(status(&args.transfer_id, session))
            }
            "transfer_upload_complete" => {
                let args: Id = serde_json::from_value(arguments)?;
                let session = sessions
                    .get_mut(&args.transfer_id)
                    .context("transfer expired or not found")?;
                let Transfer::Upload(upload) = &mut session.body else {
                    bail!("not an upload transfer")
                };
                if !upload.complete {
                    ensure!(upload.written == session.size, "upload is incomplete");
                    let hash = format!("{:x}", upload.hash.clone().finalize());
                    ensure!(
                        upload
                            .expected_hash
                            .as_ref()
                            .is_none_or(|expected| expected == &hash),
                        "SHA-256 mismatch; destination was not changed"
                    );
                    let file = upload.file.as_mut().context("upload file is closed")?;
                    ensure!(
                        file.metadata()?.len() == session.size,
                        "temporary file size changed"
                    );
                    file.sync_all()?;
                    if upload.overwrite {
                        match upload.parent.symlink_metadata(&upload.name) {
                            Ok(meta) => ensure!(
                                meta.is_file() && !meta.file_type().is_symlink(),
                                "destination must be a regular file, not a symlink"
                            ),
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                            Err(e) => return Err(e.into()),
                        }
                        upload
                            .parent
                            .rename(&upload.temporary, &upload.parent, &upload.name)?;
                    } else {
                        // hard_link fails atomically if a destination appeared during transfer.
                        upload
                            .parent
                            .hard_link(&upload.temporary, &upload.parent, &upload.name)?;
                        let _ = upload.parent.remove_file(&upload.temporary);
                    }
                    upload.complete = true;
                    upload.file = None;
                }
                Ok(status(&args.transfer_id, session))
            }
            "transfer_status" => {
                let args: Id = serde_json::from_value(arguments)?;
                let session = sessions
                    .get(&args.transfer_id)
                    .context("transfer expired or not found")?;
                Ok(status(&args.transfer_id, session))
            }
            "transfer_cancel" => {
                let args: Id = serde_json::from_value(arguments)?;
                sessions.remove(&args.transfer_id);
                Ok(json!({"cancelled":true}))
            }
            _ => bail!("unknown transfer operation"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cap_std::ambient_authority;
    fn tools(root: &Path, write: bool) -> TransferTools {
        TransferTools::new(
            Dir::open_ambient_dir(root, ambient_authority()).unwrap(),
            write,
        )
    }
    fn begin(t: &TransferTools, size: u64) -> Value {
        t.execute(
            "transfer_upload_begin",
            json!({"path":"result.bin","size":size}),
        )
        .unwrap()
    }
    #[test]
    fn binary_round_trip_retry_hash_atomicity_and_no_clobber() {
        let root = tempfile::tempdir().unwrap();
        let t = tools(root.path(), true);
        let bytes = [0, 255, 1, 2, 128];
        let s = begin(&t, bytes.len() as u64);
        let id = s["transfer_id"].as_str().unwrap();
        assert!(!root.path().join("result.bin").exists());
        let chunk = json!({"transfer_id":id,"offset":0,"data":STANDARD.encode(bytes)});
        t.execute("transfer_upload_chunk", chunk.clone()).unwrap();
        t.execute("transfer_upload_chunk", chunk).unwrap();
        assert!(
            t.execute(
                "transfer_upload_chunk",
                json!({"transfer_id":id,"offset":0,"data":STANDARD.encode([4,3])})
            )
            .is_err()
        );
        let result = t
            .execute("transfer_upload_complete", json!({"transfer_id":id}))
            .unwrap();
        assert_eq!(result["sha256"], format!("{:x}", Sha256::digest(bytes)));
        t.execute("transfer_upload_complete", json!({"transfer_id":id}))
            .unwrap();
        assert_eq!(
            std::fs::read(root.path().join("result.bin")).unwrap(),
            bytes
        );
        assert!(
            t.execute(
                "transfer_upload_begin",
                json!({"path":"result.bin","size":0})
            )
            .is_err()
        );
        let d = t
            .execute("transfer_download_begin", json!({"path":"result.bin"}))
            .unwrap();
        let chunk = t
            .execute(
                "transfer_download_chunk",
                json!({"transfer_id":d["transfer_id"],"offset":1,"length":3}),
            )
            .unwrap();
        assert_eq!(
            STANDARD.decode(chunk["data"].as_str().unwrap()).unwrap(),
            [255, 1, 2]
        );
    }
    #[test]
    fn size_permission_path_and_completion_boundaries() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            tools(root.path(), false)
                .execute("transfer_upload_begin", json!({"path":"x","size":0}))
                .is_err()
        );
        let t = tools(root.path(), true);
        for path in ["../outside", "/absolute", ""] {
            assert!(
                t.execute("transfer_upload_begin", json!({"path":path,"size":0}))
                    .is_err()
            );
        }
        assert!(
            t.execute(
                "transfer_upload_begin",
                json!({"path":"x","size":MAX_TRANSFER_BYTES+1})
            )
            .is_err()
        );
        let s = begin(&t, MAX_TRANSFER_BYTES);
        assert!(
            t.execute(
                "transfer_upload_complete",
                json!({"transfer_id":s["transfer_id"]})
            )
            .is_err()
        );
        assert!(
            t.execute(
                "transfer_upload_chunk",
                json!({"transfer_id":s["transfer_id"],"offset":MAX_TRANSFER_BYTES,"data":"AA=="})
            )
            .is_err()
        );
        t.execute("transfer_cancel", json!({"transfer_id":s["transfer_id"]}))
            .unwrap();
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        let s = begin(&t, 0);
        t.execute(
            "transfer_upload_complete",
            json!({"transfer_id":s["transfer_id"]}),
        )
        .unwrap();
        assert_eq!(
            std::fs::metadata(root.path().join("result.bin"))
                .unwrap()
                .len(),
            0
        );
    }
    #[test]
    fn pinned_download_size_cap_changed_source_and_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("large.bin");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_TRANSFER_BYTES).unwrap();
        let t = tools(root.path(), true);
        let s = t
            .execute("transfer_download_begin", json!({"path":"large.bin"}))
            .unwrap();
        t.execute(
            "transfer_download_chunk",
            json!({"transfer_id":s["transfer_id"],"offset":MAX_TRANSFER_BYTES-1,"length":1}),
        )
        .unwrap();
        file.set_len(MAX_TRANSFER_BYTES + 1).unwrap();
        assert!(
            t.execute("transfer_download_begin", json!({"path":"large.bin"}))
                .is_err()
        );
        assert!(
            t.execute(
                "transfer_download_chunk",
                json!({"transfer_id":s["transfer_id"],"offset":0,"length":1})
            )
            .is_err()
        );
        let upload = begin(&t, 1);
        t.sessions
            .lock()
            .unwrap()
            .get_mut(upload["transfer_id"].as_str().unwrap())
            .unwrap()
            .created = Instant::now() - Duration::from_secs(TRANSFER_TTL_SECONDS + 1);
        t.cleanup();
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
    #[test]
    fn hash_mismatch_and_concurrent_destination_preserve_existing_data() {
        let root = tempfile::tempdir().unwrap();
        let t = tools(root.path(), true);
        let s = t
            .execute(
                "transfer_upload_begin",
                json!({"path":"result.bin","size":1,"sha256":"0".repeat(64)}),
            )
            .unwrap();
        t.execute(
            "transfer_upload_chunk",
            json!({"transfer_id":s["transfer_id"],"offset":0,"data":"AA=="}),
        )
        .unwrap();
        assert!(
            t.execute(
                "transfer_upload_complete",
                json!({"transfer_id":s["transfer_id"]})
            )
            .is_err()
        );
        assert!(!root.path().join("result.bin").exists());
        t.execute("transfer_cancel", json!({"transfer_id":s["transfer_id"]}))
            .unwrap();
        let s = begin(&t, 0);
        std::fs::write(root.path().join("result.bin"), b"keep").unwrap();
        assert!(
            t.execute(
                "transfer_upload_complete",
                json!({"transfer_id":s["transfer_id"]})
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(root.path().join("result.bin")).unwrap(),
            b"keep"
        );
    }
    #[cfg(unix)]
    #[test]
    fn refuses_symlink_sources_and_destinations() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("private"), b"keep").unwrap();
        std::os::unix::fs::symlink(outside.path().join("private"), root.path().join("link"))
            .unwrap();
        let t = tools(root.path(), true);
        assert!(
            t.execute("transfer_download_begin", json!({"path":"link"}))
                .is_err()
        );
        assert!(
            t.execute(
                "transfer_upload_begin",
                json!({"path":"link","size":0,"overwrite":true})
            )
            .is_err()
        );
    }
    #[test]
    #[ignore = "writes and reads a complete 1 GiB file; run explicitly with --release"]
    fn full_gib_native_transfer_integrity() {
        let root = tempfile::tempdir().unwrap();
        let t = tools(root.path(), true);
        let mut block: Vec<u8> = (0..TRANSFER_CHUNK_BYTES)
            .map(|i| (i.wrapping_mul(31) ^ (i >> 8)) as u8)
            .collect();
        let mut expected = Sha256::new();
        for offset in (0..MAX_TRANSFER_BYTES).step_by(TRANSFER_CHUNK_BYTES) {
            block[..8].copy_from_slice(&offset.to_le_bytes());
            expected.update(&block);
        }
        let digest = format!("{:x}", expected.finalize());
        let started = t
            .execute(
                "transfer_upload_begin",
                json!({
                    "path":"full-gib.bin", "size":MAX_TRANSFER_BYTES, "sha256":digest
                }),
            )
            .unwrap();
        let upload = started["transfer_id"].as_str().unwrap();
        for offset in (0..MAX_TRANSFER_BYTES).step_by(TRANSFER_CHUNK_BYTES) {
            block[..8].copy_from_slice(&offset.to_le_bytes());
            let result = t
                .execute(
                    "transfer_upload_chunk",
                    json!({
                        "transfer_id":upload, "offset":offset, "data":STANDARD.encode(&block)
                    }),
                )
                .unwrap();
            assert_eq!(
                result["bytes_received"],
                offset + TRANSFER_CHUNK_BYTES as u64
            );
        }
        let result = t
            .execute("transfer_upload_complete", json!({"transfer_id":upload}))
            .unwrap();
        assert_eq!(result["sha256"], digest);
        assert_eq!(
            std::fs::metadata(root.path().join("full-gib.bin"))
                .unwrap()
                .len(),
            MAX_TRANSFER_BYTES
        );
        let started = t
            .execute("transfer_download_begin", json!({"path":"full-gib.bin"}))
            .unwrap();
        let download = started["transfer_id"].as_str().unwrap();
        let mut received = Sha256::new();
        let mut length = 0u64;
        for offset in (0..MAX_TRANSFER_BYTES).step_by(TRANSFER_CHUNK_BYTES) {
            let result = t
                .execute(
                    "transfer_download_chunk",
                    json!({
                        "transfer_id":download, "offset":offset, "length":TRANSFER_CHUNK_BYTES
                    }),
                )
                .unwrap();
            let bytes = STANDARD.decode(result["data"].as_str().unwrap()).unwrap();
            block[..8].copy_from_slice(&offset.to_le_bytes());
            assert_eq!(bytes, block);
            length += bytes.len() as u64;
            received.update(&bytes);
        }
        assert_eq!(length, MAX_TRANSFER_BYTES);
        assert_eq!(format!("{:x}", received.finalize()), digest);
        t.execute("transfer_cancel", json!({"transfer_id":download}))
            .unwrap();
        t.execute("transfer_cancel", json!({"transfer_id":upload}))
            .unwrap();
        println!(
            "Verified {length} bytes through native upload, atomic completion and download; SHA-256 {digest}"
        );
    }
}
