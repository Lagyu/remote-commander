use anyhow::{Context, Result, bail, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use rdc_protocol::{READ_LIMIT, WRITE_LIMIT};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant, UNIX_EPOCH},
};

const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;
const MAX_DOWNLOAD_CHUNK_BYTES: usize = 256 * 1024;
const MAX_ENTRIES: usize = 2_000;

pub struct FileTools {
    dir: Dir,
    pub root: PathBuf,
    pub allow_write: bool,
    searches: Mutex<BTreeMap<String, Search>>,
    pub transfers: crate::transfers::TransferTools,
}

struct Search {
    created: Instant,
    results: Vec<Value>,
    truncated: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathArgs {
    path: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    path: String,
    #[serde(default)]
    offset: usize,
    #[serde(default = "default_lines")]
    length: usize,
}
fn default_lines() -> usize {
    200
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DownloadChunkArgs {
    path: String,
    offset: u64,
    length: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    path: String,
    #[serde(default = "one")]
    depth: usize,
}
fn one() -> usize {
    1
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    path: String,
    content: String,
    #[serde(default = "rewrite")]
    mode: String,
}
fn rewrite() -> String {
    "rewrite".into()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditArgs {
    path: String,
    old_string: String,
    new_string: String,
    #[serde(default = "one")]
    expected_replacements: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MoveArgs {
    source: String,
    destination: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MultipleArgs {
    paths: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    path: String,
    pattern: String,
    #[serde(default = "files")]
    search_type: String,
}
fn files() -> String {
    "files".into()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchPage {
    search_id: String,
    #[serde(default)]
    offset: usize,
    #[serde(default = "page_size")]
    length: usize,
}
fn page_size() -> usize {
    50
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchId {
    search_id: String,
}

pub fn relative(path: &str) -> Result<&Path> {
    ensure!(path.len() <= 4096, "path is too long");
    let p = Path::new(path);
    ensure!(
        !path.is_empty() && !path.contains('\0'),
        "path must not be empty or contain NUL"
    );
    ensure!(
        p.components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir)),
        "path must be relative and cannot contain '..'"
    );
    Ok(p)
}

impl FileTools {
    pub fn new(root: &Path, allow_write: bool) -> Result<Self> {
        let root = root
            .canonicalize()
            .context("root must be an existing directory")?;
        let dir = Dir::open_ambient_dir(&root, ambient_authority())?;
        let transfers = crate::transfers::TransferTools::new(dir.try_clone()?, allow_write);
        Ok(Self {
            dir,
            root,
            transfers,
            allow_write,
            searches: Mutex::new(BTreeMap::new()),
        })
    }

    fn text(&self, path: &str) -> Result<String> {
        let path = relative(path)?;
        ensure!(
            self.dir.metadata(path)?.is_file(),
            "expected a regular file"
        );
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt;
            // A concurrent replacement with a FIFO must not block a worker
            // thread indefinitely. Regular-file reads ignore O_NONBLOCK.
            options.custom_flags(libc::O_NONBLOCK);
        }
        let mut file = self
            .dir
            .open_with(path, &options)
            .context("cannot open file within root")?;
        let meta = file.metadata()?;
        ensure!(meta.is_file(), "expected a regular file");
        ensure!(
            meta.len() <= MAX_FILE_BYTES as u64,
            "file exceeds the 2 MiB text-tool limit"
        );
        let mut bytes = Vec::new();
        (&mut file)
            .take((MAX_FILE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= MAX_FILE_BYTES,
            "file grew beyond the text-tool limit"
        );
        String::from_utf8(bytes).context("file is not UTF-8 text")
    }

    fn download_chunk(&self, args: DownloadChunkArgs) -> Result<Value> {
        ensure!(
            args.length > 0 && args.length <= MAX_DOWNLOAD_CHUNK_BYTES,
            "download chunk length must be 1–256 KiB"
        );
        let path = relative(&args.path)?;
        let entry = self.dir.symlink_metadata(path)?;
        ensure!(
            entry.is_file() && !entry.file_type().is_symlink(),
            "expected a regular file"
        );
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK);
        }
        let mut file = self
            .dir
            .open_with(path, &options)
            .context("cannot open file within root")?;
        let meta = file.metadata()?;
        ensure!(meta.is_file(), "expected a regular file");
        ensure!(
            args.offset <= meta.len(),
            "download offset exceeds file size"
        );
        file.seek(SeekFrom::Start(args.offset))?;
        let mut bytes = Vec::with_capacity(args.length);
        (&mut file)
            .take(args.length as u64)
            .read_to_end(&mut bytes)?;
        let next = args.offset + bytes.len() as u64;
        Ok(json!({
            "path": args.path,
            "offset": args.offset,
            "size": meta.len(),
            "bytes": bytes.len(),
            "data": STANDARD.encode(&bytes),
            "next_offset": if next < meta.len() { Some(next) } else { None },
        }))
    }

    fn read(&self, args: ReadArgs, limit: usize) -> Result<Value> {
        ensure!(
            args.length > 0 && args.length <= 2000 && args.offset <= 1_000_000,
            "invalid line range"
        );
        let text = self.text(&args.path)?;
        let lines: Vec<_> = text.split_inclusive('\n').collect();
        let mut content = String::new();
        let mut next = args.offset.min(lines.len());
        for line in lines.iter().skip(args.offset).take(args.length) {
            if content.len() + line.len() > limit {
                ensure!(
                    !content.is_empty(),
                    "one line exceeds this request's output limit"
                );
                break;
            }
            content.push_str(line);
            next += 1;
        }
        Ok(
            json!({"path":args.path,"content":content,"offset":args.offset,"next_offset":if next < lines.len() { Some(next) } else { None },"total_lines":lines.len()}),
        )
    }

    // Dir is a capability: resolution remains beneath the open directory even if
    // another process swaps a parent directory for a symlink during an operation.
    fn atomic_write(&self, path: &str, content: &[u8]) -> Result<()> {
        let path = relative(path)?;
        ensure!(path.file_name().is_some(), "expected a file name");
        match self.dir.symlink_metadata(path) {
            Ok(meta) => ensure!(
                meta.is_file() && !meta.file_type().is_symlink(),
                "destination must be a regular file"
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let target_dir = self.dir.open_dir(parent)?;
        let name = path.file_name().unwrap();
        let temporary = format!(".rdc-{}.tmp", uuid::Uuid::new_v4());
        let result = (|| -> Result<()> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use cap_std::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = target_dir.open_with(&temporary, &options)?;
            file.write_all(content)?;
            file.sync_all()?;
            target_dir.rename(&temporary, &target_dir, name)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = target_dir.remove_file(&temporary);
        }
        result
    }

    fn walk(&self, path: &str, max_depth: usize) -> Result<(Vec<(String, bool)>, bool)> {
        relative(path)?;
        let started = Instant::now();
        let mut pending = vec![(path.to_owned(), 0usize)];
        let mut entries = Vec::new();
        let mut truncated = false;
        while let Some((parent, depth)) = pending.pop() {
            for entry in self.dir.open_dir(&parent)?.entries()? {
                if entries.len() >= MAX_ENTRIES || started.elapsed() > Duration::from_secs(2) {
                    truncated = true;
                    break;
                }
                let entry = entry?;
                let kind = entry.file_type()?;
                if kind.is_symlink() || (!kind.is_dir() && !kind.is_file()) {
                    continue;
                }
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let child = Path::new(&parent).join(name).to_string_lossy().into_owned();
                entries.push((child.clone(), kind.is_dir()));
                if kind.is_dir() && depth + 1 < max_depth {
                    pending.push((child, depth + 1));
                }
            }
            if truncated {
                break;
            }
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        Ok((entries, truncated))
    }

    fn search(&self, args: SearchArgs) -> Result<Value> {
        ensure!(
            !args.pattern.is_empty() && args.pattern.len() <= 256,
            "pattern must contain 1–256 bytes"
        );
        ensure!(
            args.search_type == "files" || args.search_type == "content",
            "unknown search_type"
        );
        let started = Instant::now();
        let (files, mut truncated) = self.walk(&args.path, 16)?;
        let mut results = Vec::new();
        let mut scanned = 0usize;
        for (path, directory) in files {
            if results.len() >= 1000
                || scanned > 8 * 1024 * 1024
                || started.elapsed() > Duration::from_secs(3)
            {
                truncated = true;
                break;
            }
            if args.search_type == "files" {
                if path.contains(&args.pattern) {
                    results.push(json!({"path":path,"is_directory":directory}));
                }
            } else if !directory {
                let Ok(text) = self.text(&path) else {
                    continue;
                };
                scanned += text.len();
                for (line, content) in text.lines().enumerate() {
                    if content.contains(&args.pattern) {
                        results.push(json!({"path":path,"line":line+1,"text":content.chars().take(300).collect::<String>()}));
                        if results.len() >= 1000 {
                            truncated = true;
                            break;
                        }
                    }
                }
            }
        }
        let id = uuid::Uuid::new_v4().to_string();
        let mut searches = self.searches.lock().unwrap();
        searches.retain(|_, search| search.created.elapsed() < Duration::from_secs(600));
        ensure!(
            searches.len() < 8,
            "at most 8 search snapshots may be retained; stop an old search first"
        );
        let page = json!({"search_id":id,"results":results.iter().take(50).collect::<Vec<_>>(),"total":results.len(),"next_offset":if results.len() > 50 { Some(50) } else { None },"truncated":truncated});
        searches.insert(
            id,
            Search {
                created: Instant::now(),
                results,
                truncated,
            },
        );
        Ok(page)
    }

    pub fn execute(&self, name: &str, arguments: Value) -> Result<Value> {
        if crate::transfers::internal_tool(name) {
            return self.transfers.execute(name, arguments);
        }
        if rdc_protocol::scope_for(name) == Some(rdc_protocol::SCOPES[1]) {
            ensure!(
                self.allow_write,
                "file writes are disabled; restart the agent with --allow-write to enable them"
            );
        }
        match name {
            "ping_device" => Ok(json!({"ok":true,"platform":std::env::consts::OS})),
            "list_directory" => {
                let args: ListArgs = serde_json::from_value(arguments)?;
                ensure!((1..=4).contains(&args.depth), "depth must be 1–4");
                let (entries, truncated) = self.walk(&args.path, args.depth)?;
                Ok(
                    json!({"entries":entries.into_iter().map(|(path,is_directory)| json!({"path":path,"is_directory":is_directory})).collect::<Vec<_>>(),"truncated":truncated}),
                )
            }
            "read_file" => self.read(serde_json::from_value(arguments)?, READ_LIMIT),
            "download_file_chunk" => self.download_chunk(serde_json::from_value(arguments)?),
            "read_multiple_files" => {
                let args: MultipleArgs = serde_json::from_value(arguments)?;
                ensure!(
                    !args.paths.is_empty() && args.paths.len() <= 8,
                    "request between 1 and 8 paths"
                );
                let limit = READ_LIMIT / args.paths.len();
                let files: Vec<_> = args
                    .paths
                    .into_iter()
                    .map(|path| {
                        self.read(
                            ReadArgs {
                                path: path.clone(),
                                offset: 0,
                                length: 200,
                            },
                            limit,
                        )
                        .unwrap_or_else(|e| json!({"path":path,"error":e.to_string()}))
                    })
                    .collect();
                Ok(json!({"files":files}))
            }
            "get_file_info" => {
                let args: PathArgs = serde_json::from_value(arguments)?;
                let meta = self.dir.metadata(relative(&args.path)?)?;
                Ok(
                    json!({"path":args.path,"is_directory":meta.is_dir(),"is_file":meta.is_file(),"size":meta.len(),"modified_unix":meta.modified().ok().and_then(|v|v.into_std().duration_since(UNIX_EPOCH).ok()).map(|v|v.as_secs())}),
                )
            }
            "write_file" => {
                let args: WriteArgs = serde_json::from_value(arguments)?;
                ensure!(args.content.len() <= WRITE_LIMIT, "content exceeds 64 KiB");
                ensure!(
                    args.mode == "rewrite" || args.mode == "append",
                    "mode must be rewrite or append"
                );
                let content = if args.mode == "append" {
                    let mut current = self.text(&args.path)?;
                    ensure!(
                        current.len() + args.content.len() <= MAX_FILE_BYTES,
                        "result exceeds the text-tool limit"
                    );
                    current.push_str(&args.content);
                    current
                } else {
                    args.content
                };
                self.atomic_write(&args.path, content.as_bytes())?;
                Ok(json!({"path":args.path,"bytes":content.len()}))
            }
            "edit_block" => {
                let args: EditArgs = serde_json::from_value(arguments)?;
                ensure!(
                    !args.old_string.is_empty() && (1..=100).contains(&args.expected_replacements),
                    "invalid replacement precondition"
                );
                ensure!(
                    args.new_string.len() <= WRITE_LIMIT,
                    "replacement exceeds 64 KiB"
                );
                let text = self.text(&args.path)?;
                let count = text.matches(&args.old_string).count();
                ensure!(
                    count == args.expected_replacements,
                    "expected {} occurrences, found {}; no change made",
                    args.expected_replacements,
                    count
                );
                let result_len =
                    text.len() - count * args.old_string.len() + count * args.new_string.len();
                ensure!(
                    result_len <= MAX_FILE_BYTES,
                    "edited file exceeds the text-tool limit"
                );
                let result = text.replace(&args.old_string, &args.new_string);
                self.atomic_write(&args.path, result.as_bytes())?;
                Ok(json!({"path":args.path,"replacements":count}))
            }
            "create_directory" => {
                let args: PathArgs = serde_json::from_value(arguments)?;
                self.dir.create_dir_all(relative(&args.path)?)?;
                Ok(json!({"path":args.path,"created":true}))
            }
            "move_file" => {
                let args: MoveArgs = serde_json::from_value(arguments)?;
                let source = relative(&args.source)?;
                let destination = relative(&args.destination)?;
                let meta = self.dir.symlink_metadata(source)?;
                ensure!(
                    meta.is_file() && !meta.file_type().is_symlink(),
                    "only regular files can be moved"
                );
                // hard_link is atomic and fails if destination exists, unlike rename.
                self.dir.hard_link(source, &self.dir, destination)?;
                if let Err(error) = self.dir.remove_file(source) {
                    let _ = self.dir.remove_file(destination);
                    return Err(error.into());
                }
                Ok(json!({"source":args.source,"destination":args.destination}))
            }
            "start_search" => self.search(serde_json::from_value(arguments)?),
            "get_more_search_results" => {
                let args: SearchPage = serde_json::from_value(arguments)?;
                ensure!(
                    args.length > 0 && args.length <= 100 && args.offset <= 1000,
                    "invalid page range"
                );
                let mut searches = self.searches.lock().unwrap();
                searches.retain(|_, x| x.created.elapsed() < Duration::from_secs(600));
                let search = searches
                    .get(&args.search_id)
                    .context("search expired or not found")?;
                let next = (args.offset + args.length).min(search.results.len());
                Ok(
                    json!({"search_id":args.search_id,"results":search.results.iter().skip(args.offset).take(args.length).collect::<Vec<_>>(),"next_offset":if next < search.results.len() { Some(next) } else { None },"total":search.results.len(),"truncated":search.truncated}),
                )
            }
            "stop_search" => {
                let args: SearchId = serde_json::from_value(arguments)?;
                Ok(
                    json!({"removed":self.searches.lock().unwrap().remove(&args.search_id).is_some()}),
                )
            }
            "list_searches" => {
                let mut searches = self.searches.lock().unwrap();
                searches.retain(|_, x| x.created.elapsed() < Duration::from_secs(600));
                Ok(
                    json!({"searches":searches.iter().map(|(id,x)|json!({"search_id":id,"total":x.results.len()})).collect::<Vec<_>>()}),
                )
            }
            _ => bail!("unknown file tool"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capability_blocks_traversal_and_symlink_escape_for_reads_and_writes() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("private.txt"), "private").unwrap();
        let fs = FileTools::new(root.path(), true).unwrap();
        assert!(
            fs.execute("read_file", json!({"path":"../private.txt"}))
                .is_err()
        );
        assert!(
            fs.execute(
                "write_file",
                json!({"path":"../private.txt","content":"bad"})
            )
            .is_err()
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
            assert!(
                fs.execute("read_file", json!({"path":"escape/private.txt"}))
                    .is_err()
            );
            assert!(
                fs.execute(
                    "write_file",
                    json!({"path":"escape/private.txt","content":"bad"})
                )
                .is_err()
            );
            assert!(
                fs.execute("create_directory", json!({"path":"escape/new"}))
                    .is_err()
            );
        }
        assert_eq!(
            std::fs::read_to_string(outside.path().join("private.txt")).unwrap(),
            "private"
        );
    }

    #[test]
    fn edits_check_preconditions_and_moves_never_overwrite() {
        let root = tempfile::tempdir().unwrap();
        let fs = FileTools::new(root.path(), true).unwrap();
        fs.execute("write_file", json!({"path":"a","content":"hello hello"}))
            .unwrap();
        assert!(
            fs.execute(
                "edit_block",
                json!({"path":"a","old_string":"hello","new_string":"bye"})
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("a")).unwrap(),
            "hello hello"
        );
        fs.execute("write_file", json!({"path":"b","content":"keep"}))
            .unwrap();
        assert!(
            fs.execute("move_file", json!({"source":"a","destination":"b"}))
                .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("b")).unwrap(),
            "keep"
        );
        fs.execute("move_file", json!({"source":"a","destination":"c"}))
            .unwrap();
        assert!(!root.path().join("a").exists());
        assert_eq!(
            std::fs::read_to_string(root.path().join("c")).unwrap(),
            "hello hello"
        );
    }

    #[test]
    fn writes_require_explicit_local_permission() {
        let root = tempfile::tempdir().unwrap();
        let fs = FileTools::new(root.path(), false).unwrap();
        assert!(
            fs.execute("write_file", json!({"path":"a","content":"bad"}))
                .is_err()
        );
        assert!(!root.path().join("a").exists());
    }
}
