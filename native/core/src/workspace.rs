use anyhow::{bail, ensure, Context, Result};
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

pub const MAX_FILE_BYTES: usize = 4_000_000;

/// Directory capabilities make file operations relative to an already-open
/// workspace handle. Symlink traversal cannot escape that directory.
pub struct Workspace {
    pub path: PathBuf,
    dir: Dir,
}

#[derive(Clone, Debug, Serialize)]
pub struct FileContent {
    pub path: String,
    pub content: String,
    pub hash: String,
    pub bytes: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub bytes: Option<Vec<u8>>,
    pub mode: Option<u32>,
    pub hash: Option<String>,
}

/// Stable reservation key for an existing workspace or a managed checkout whose
/// final directory was removed. Never resolve a missing ancestor or traversal.
pub(crate) fn reservation_path(path: &Path) -> Result<PathBuf> {
    match path.canonicalize() {
        Ok(path) => {
            ensure!(path.is_dir(), "Workspace must be a directory");
            Ok(path)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ensure!(
                path.is_absolute()
                    && path.components().all(|part| matches!(
                        part,
                        Component::RootDir | Component::Normal(_) | Component::Prefix(_)
                    )),
                "Missing workspace reservation must use an absolute normalized path"
            );
            ensure!(
                matches!(std::fs::symlink_metadata(path), Err(ref error) if error.kind() == std::io::ErrorKind::NotFound),
                "Missing workspace must not be a dangling symbolic link"
            );
            let parent = path
                .parent()
                .context("Missing workspace parent")?
                .canonicalize()?;
            ensure!(parent.is_dir(), "Workspace parent must be a directory");
            Ok(parent.join(path.file_name().context("Missing workspace name")?))
        }
        Err(error) => Err(error.into()),
    }
}

impl Workspace {
    pub fn open(path: &Path) -> Result<Self> {
        let path = path.canonicalize().context("Workspace does not exist")?;
        ensure!(path.is_dir(), "Workspace must be a directory");
        let dir = Dir::open_ambient_dir(&path, ambient_authority())?;
        Ok(Self { path, dir })
    }
    pub fn relative(&self, path: &str) -> Result<PathBuf> {
        ensure!(!path.contains('\0'), "Path contains a NUL byte");
        let path = Path::new(path);
        let path = if path.is_absolute() {
            path.strip_prefix(&self.path)
                .context("Path is outside the workspace")?
        } else {
            path
        };
        let mut normalized = PathBuf::new();
        for component in path.components() {
            match component {
                Component::Normal(part) => normalized.push(part),
                Component::CurDir => {}
                Component::ParentDir => {
                    ensure!(normalized.pop(), "Path escapes the workspace");
                }
                _ => bail!("Invalid workspace path"),
            }
        }
        Ok(if normalized.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            normalized
        })
    }
    pub(crate) fn writable(&self, path: &str) -> Result<PathBuf> {
        let rel = self.relative(path)?;
        ensure!(rel != Path::new("."), "Cannot modify the workspace root");
        ensure!(
            !rel.components().any(|c| c.as_os_str() == ".git"),
            "Use Git tools to modify repository metadata"
        );
        // Reading a confined symlink is useful; replacing it silently changes
        // the link itself and makes a content-only checkpoint lossy.
        let mut part = PathBuf::new();
        for component in rel.components() {
            part.push(component);
            match self.dir.symlink_metadata(&part) {
                Ok(meta) => ensure!(
                    !meta.file_type().is_symlink(),
                    "Edits through symlinks are not supported; use the real workspace path"
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(error) => return Err(error.into()),
            }
        }
        let mut ancestor = rel
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        loop {
            match self.dir.canonicalize(ancestor) {
                Ok(resolved) => {
                    ensure!(
                        !resolved.components().any(|c| c.as_os_str() == ".git"),
                        "Use Git tools to modify repository metadata"
                    );
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    ancestor = ancestor
                        .parent()
                        .filter(|p| !p.as_os_str().is_empty())
                        .unwrap_or_else(|| Path::new("."));
                }
                Err(error) => {
                    return Err(error).context("Destination is not confined to the workspace")
                }
            }
        }
        Ok(rel)
    }
    pub fn list(&self, path: &str) -> Result<Vec<FileEntry>> {
        let rel = self.relative(path)?;
        let mut entries = Vec::new();
        for entry in self.dir.read_dir(&rel)? {
            let entry = entry?;
            if entries.len() >= 10_000 {
                bail!("Directory has too many entries; narrow the path");
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == ".git" {
                continue;
            }
            let child = rel.join(&name);
            let kind = if entry.file_type()?.is_dir() {
                "dir"
            } else {
                "file"
            };
            entries.push(FileEntry {
                name,
                path: child
                    .strip_prefix(".")
                    .unwrap_or(&child)
                    .to_string_lossy()
                    .into_owned(),
                kind: kind.into(),
            });
        }
        entries.sort_by(|a, b| {
            (a.kind != "dir", a.name.to_lowercase()).cmp(&(b.kind != "dir", b.name.to_lowercase()))
        });
        Ok(entries)
    }
    pub fn snapshot(&self, path: &str) -> Result<Snapshot> {
        let rel = self.relative(path)?;
        let file = match self.dir.open(&rel) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Snapshot {
                    bytes: None,
                    mode: None,
                    hash: None,
                })
            }
            Err(error) => return Err(error).context("Cannot open file within the workspace"),
        };
        let meta = file.metadata()?;
        ensure!(meta.is_file(), "Not a regular file");
        ensure!(
            meta.len() <= MAX_FILE_BYTES as u64,
            "File exceeds the 4 MB edit limit"
        );
        #[cfg(unix)]
        let mode = {
            use cap_std::fs::MetadataExt;
            Some(meta.mode() & 0o777)
        };
        #[cfg(not(unix))]
        let mode = None;
        let mut bytes = Vec::new();
        file.take((MAX_FILE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= MAX_FILE_BYTES,
            "File grew beyond the edit limit"
        );
        Ok(Snapshot {
            hash: Some(hash(&bytes)),
            bytes: Some(bytes),
            mode,
        })
    }
    /// A bounded inspection read. Nonblocking/no-follow open avoids hanging on
    /// a FIFO or following a symlink swapped in after directory discovery.
    pub(crate) fn inspect(&self, path: &str, limit: usize) -> Result<(Vec<u8>, u64)> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
        }
        let relative = self.relative(path)?;
        #[cfg(unix)]
        let file = {
            use cap_std::fs::OpenOptionsExt;
            let mut parent = self.dir.try_clone()?;
            let mut directory = OpenOptions::new();
            directory
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK);
            for part in relative.parent().unwrap_or(Path::new(".")).components() {
                if let Component::Normal(name) = part {
                    parent = Dir::from_std_file(parent.open_with(name, &directory)?.into_std());
                }
            }
            parent.open_with(
                relative
                    .file_name()
                    .context("Inspection requires a file path")?,
                &options,
            )?
        };
        #[cfg(not(unix))]
        let file = self.dir.open_with(relative, &options)?;
        let meta = file.metadata()?;
        ensure!(meta.is_file(), "Inspection requires a regular file");
        let mut bytes = Vec::new();
        file.take(limit as u64).read_to_end(&mut bytes)?;
        Ok((bytes, meta.len()))
    }
    pub fn read(&self, path: &str) -> Result<FileContent> {
        let snapshot = self.snapshot(path)?;
        let bytes = snapshot.bytes.context("File not found")?;
        ensure!(
            !bytes.contains(&0),
            "Binary files cannot be displayed as text"
        );
        Ok(FileContent {
            path: self.relative(path)?.to_string_lossy().into_owned(),
            hash: hash(&bytes),
            bytes: bytes.len(),
            content: String::from_utf8(bytes).context("File is not valid UTF-8")?,
        })
    }
    /// Keep a directory capability alive while a native library reads a file
    /// and its adjacent sidecars. Every relative parent must be a real directory.
    #[cfg(unix)]
    pub(crate) fn confined_parent(&self, path: &str) -> Result<(Dir, String)> {
        use cap_std::fs::OpenOptionsExt;
        let relative = self.relative(path)?;
        let name = relative
            .file_name()
            .and_then(|n| n.to_str())
            .context("A regular file path with a UTF-8 name is required")?
            .to_owned();
        let mut parent = self.dir.try_clone()?;
        let mut options = OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK);
        for part in relative.parent().unwrap_or(Path::new(".")).components() {
            if let Component::Normal(name) = part {
                parent = Dir::from_std_file(
                    parent
                        .open_with(name, &options)
                        .context("Database parent must be a real workspace directory")?
                        .into_std(),
                );
            }
        }
        Ok((parent, name))
    }
    pub fn write(&self, path: &str, bytes: &[u8], expected: Option<&str>) -> Result<String> {
        ensure!(
            bytes.len() <= MAX_FILE_BYTES,
            "File exceeds the 4 MB edit limit"
        );
        let rel = self.writable(path)?;
        let before = self.snapshot(path)?;
        if let Some(expected) = expected {
            ensure!(
                before.hash.as_deref().unwrap_or("missing") == expected,
                "File changed since it was read; inspect it again before editing"
            );
        }
        let parent = rel
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        self.dir.create_dir_all(parent)?;
        let parent = self.dir.open_dir(parent)?;
        let name = rel.file_name().context("Missing file name")?;
        let temporary = format!(".shadow-write-{}", crate::id());
        let result = (|| -> Result<()> {
            let mut file =
                parent.open_with(&temporary, OpenOptions::new().write(true).create_new(true))?;
            #[cfg(unix)]
            {
                use cap_std::fs::{Permissions, PermissionsExt};
                file.set_permissions(Permissions::from_mode(before.mode.unwrap_or(0o644)))?;
            }
            file.write_all(bytes)?;
            file.sync_all()?;
            parent.rename(&temporary, &parent, name)?;
            parent.open(".")?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = parent.remove_file(&temporary);
        }
        result?;
        Ok(hash(bytes))
    }
    pub fn edit(&self, path: &str, old: &str, new: &str, replace_all: bool) -> Result<String> {
        ensure!(!old.is_empty(), "Old text must not be empty");
        let file = self.read(path)?;
        let count = file.content.matches(old).count();
        ensure!(count > 0, "Old text was not found");
        ensure!(
            replace_all || count == 1,
            "Old text matches multiple locations; include more context or explicitly replace all"
        );
        let edited = if replace_all {
            file.content.replace(old, new)
        } else {
            file.content.replacen(old, new, 1)
        };
        self.write(path, edited.as_bytes(), Some(&file.hash))
    }
    pub fn mkdir(&self, path: &str) -> Result<()> {
        self.dir.create_dir_all(self.writable(path)?)?;
        Ok(())
    }
    pub(crate) fn remove_empty_dir(&self, path: &str) -> Result<()> {
        self.dir.remove_dir(self.writable(path)?)?;
        Ok(())
    }
    pub fn set_mode(&self, path: &str, mode: u32) -> Result<()> {
        #[cfg(unix)]
        {
            use cap_std::fs::{Permissions, PermissionsExt};
            let file = self.dir.open(self.writable(path)?)?;
            file.set_permissions(Permissions::from_mode(mode & 0o777))?;
        }
        Ok(())
    }
    pub fn delete(&self, path: &str, expected: Option<&str>) -> Result<()> {
        let rel = self.writable(path)?;
        let snapshot = self.snapshot(path)?;
        ensure!(snapshot.bytes.is_some(), "File not found");
        if let Some(expected) = expected {
            ensure!(
                snapshot.hash.as_deref() == Some(expected),
                "File changed since it was read"
            );
        }
        self.dir.remove_file(rel)?;
        Ok(())
    }
    pub fn move_file(&self, source: &str, destination: &str) -> Result<()> {
        let source = self.writable(source)?;
        let destination = self.writable(destination)?;
        ensure!(
            self.dir.metadata(&source)?.is_file(),
            "Move supports regular files only"
        );
        ensure!(
            self.dir.symlink_metadata(&destination).is_err(),
            "Destination already exists"
        );
        if let Some(parent) = destination.parent().filter(|p| !p.as_os_str().is_empty()) {
            self.dir.create_dir_all(parent)?;
        }
        self.dir.rename(source, &self.dir, destination)?;
        Ok(())
    }
    pub fn search(
        &self,
        pattern: &str,
        glob: Option<&str>,
        max_results: usize,
    ) -> Result<serde_json::Value> {
        self.search_with_control(
            pattern,
            glob,
            ".",
            max_results,
            &tokio_util::sync::CancellationToken::new(),
        )
    }
    pub fn search_with_control(
        &self,
        pattern: &str,
        glob: Option<&str>,
        root: &str,
        max_results: usize,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<serde_json::Value> {
        let regex = regex::RegexBuilder::new(pattern)
            .size_limit(2_000_000)
            .build()?;
        let rel_root = self.dir.canonicalize(self.relative(root)?)?;
        let mut builder = ignore::WalkBuilder::new(self.path.join(rel_root));
        builder
            .hidden(true)
            .follow_links(false)
            .max_filesize(Some(MAX_FILE_BYTES as u64));
        if let Some(glob) = glob {
            let mut overrides = ignore::overrides::OverrideBuilder::new(&self.path);
            overrides.add(glob)?;
            builder.overrides(overrides.build()?);
        }
        let mut matches = Vec::new();
        let mut scanned = 0;
        let mut scanned_bytes = 0usize;
        let limit = max_results.clamp(1, 1000);
        for entry in builder.build().filter_map(Result::ok) {
            ensure!(!cancel.is_cancelled(), "Search cancelled");
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            scanned += 1;
            if scanned > 20_000 {
                return Ok(
                    serde_json::json!({"matches":matches,"truncated":true,"scanned":scanned-1}),
                );
            }
            let rel = entry
                .path()
                .strip_prefix(&self.path)?
                .to_string_lossy()
                .into_owned();
            let Ok(file) = self.read(&rel) else { continue };
            scanned_bytes += file.bytes;
            if scanned_bytes > 128_000_000 {
                return Ok(
                    serde_json::json!({"matches":matches,"truncated":true,"scanned":scanned,"reason":"128 MB search budget reached"}),
                );
            }
            for (line, text) in file.content.lines().enumerate() {
                ensure!(!cancel.is_cancelled(), "Search cancelled");
                if regex.is_match(text) {
                    matches.push(serde_json::json!({"path":rel,"line":line+1,"text":text.chars().take(2000).collect::<String>()}));
                    if matches.len() >= limit {
                        return Ok(
                            serde_json::json!({"matches":matches,"truncated":true,"scanned":scanned}),
                        );
                    }
                }
            }
        }
        Ok(serde_json::json!({"matches":matches,"truncated":false,"scanned":scanned}))
    }
    pub fn find_files(
        &self,
        query: &str,
        root: &str,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<serde_json::Value> {
        let root = self.dir.canonicalize(self.relative(root)?)?;
        let needle = query.to_lowercase();
        let mut hits = Vec::new();
        let mut builder = ignore::WalkBuilder::new(self.path.join(root));
        builder.hidden(true).follow_links(false);
        for (scanned, entry) in builder.build().filter_map(Result::ok).enumerate() {
            ensure!(!cancel.is_cancelled(), "Search cancelled");
            if scanned >= 20_000 || hits.len() >= 200 {
                return Ok(serde_json::json!({"hits":hits,"truncated":true}));
            }
            if entry.file_type().is_some_and(|t| t.is_file())
                && entry
                    .file_name()
                    .to_string_lossy()
                    .to_lowercase()
                    .contains(&needle)
            {
                hits.push(
                    entry
                        .path()
                        .strip_prefix(&self.path)?
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
        Ok(serde_json::json!({"hits":hits,"truncated":false}))
    }
}

pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
