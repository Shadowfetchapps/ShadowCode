use anyhow::{ensure, Context, Result};
use fs2::FileExt;
use std::{
    env, fs,
    path::{Path, PathBuf},
};

/// Preserve the existing XDG identity so upgrades retain settings and history.
#[derive(Clone, Debug)]
pub struct AppPaths {
    pub config: PathBuf,
    pub data: PathBuf,
    pub state: PathBuf,
    /// Rebuildable data such as the code index (`$XDG_CACHE_HOME`); never
    /// backed up.
    pub cache: PathBuf,
}

/// A lock is released when its last legitimate owner drops, even if an
/// unrelated concurrent fork inherited the descriptor before close-on-exec.
pub struct ProfileLock {
    file: fs::File,
    owner_pid: u32,
}
impl Drop for ProfileLock {
    fn drop(&mut self) {
        // A forked copy must not unlock a still-live parent's engine.
        if self.owner_pid == std::process::id() {
            let _ = FileExt::unlock(&self.file);
        }
    }
}

impl AppPaths {
    pub fn discover() -> Result<Self> {
        let home = env::var_os("HOME").context("HOME is not set")?;
        let home = PathBuf::from(home);
        let xdg = |key: &str, fallback: &str| {
            env::var_os(key)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| home.join(fallback))
                .join("shadow-agent")
        };
        let result = Self {
            config: xdg("XDG_CONFIG_HOME", ".config"),
            data: xdg("XDG_DATA_HOME", ".local/share"),
            state: xdg("XDG_STATE_HOME", ".local/state"),
            cache: xdg("XDG_CACHE_HOME", ".cache"),
        };
        result.ensure()?;
        Ok(result)
    }

    /// Explicit roots keep tests and command-line profiles away from user data.
    pub fn isolated(root: &Path) -> Result<Self> {
        let result = Self {
            config: root.join("config"),
            data: root.join("data"),
            state: root.join("state"),
            cache: root.join("cache"),
        };
        result.ensure()?;
        Ok(result)
    }

    pub fn ensure(&self) -> Result<()> {
        for path in [&self.config, &self.data, &self.state] {
            private_directory(path)?;
        }
        Ok(())
    }
    pub fn database(&self) -> PathBuf {
        self.state.join("shadow-agent.db")
    }
    pub fn config_file(&self) -> PathBuf {
        self.config.join("config.yaml")
    }
    pub fn secrets_file(&self) -> PathBuf {
        self.config.join("secrets.env")
    }
    pub fn remembered_workspace(&self) -> Option<PathBuf> {
        let path = PathBuf::from(
            fs::read_to_string(self.state.join("last-workspace.txt"))
                .ok()?
                .trim(),
        );
        path.canonicalize().ok().filter(|p| p.is_dir())
    }
    pub fn remember_workspace(&self, path: &Path) -> Result<()> {
        atomic_write(
            &self.state.join("last-workspace.txt"),
            format!("{}\n", path.canonicalize()?.display()).as_bytes(),
            false,
        )
    }

    /// Hold this for the whole process lifetime. A second manager must not mark
    /// live jobs interrupted or overwrite another manager's workspace state.
    pub fn lock(&self) -> Result<ProfileLock> {
        self.ensure()?;
        let mut options = fs::OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options
            .open(self.state.join("native.lock"))
            .context("Cannot open the profile lock; native.lock must be a regular file")?;
        let metadata = file.metadata()?;
        ensure!(metadata.is_file(), "Profile lock must be a regular file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            ensure!(
                metadata.uid() == unsafe { libc::geteuid() } && metadata.nlink() == 1,
                "Profile lock must be owned by your account and must not have other hard links"
            );
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        file.try_lock_exclusive()
            .context("ShadowCode is already running for this profile")?;
        Ok(ProfileLock {
            file,
            owner_pid: std::process::id(),
        })
    }
}

/// Restrict only application-owned leaves, not the user's XDG base or an
/// existing --profile parent. Use the opened directory for validation/chmod so
/// a final symlink cannot redirect a permission change to another directory.
pub(crate) fn private_directory(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .with_context(|| format!("Cannot create profile directory {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        let directory = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .with_context(|| {
                format!(
                    "Profile directory {} must be a real directory, not a symlink",
                    path.display()
                )
            })?;
        ensure!(
            directory.metadata()?.uid() == unsafe { libc::geteuid() },
            "Profile directory {} must be owned by your account",
            path.display()
        );
        directory.set_permissions(fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub fn atomic_write(path: &Path, bytes: &[u8], private: bool) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().context("Path has no parent")?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if private {
            0o600
        } else {
            fs::metadata(path)
                .map(|m| m.permissions().mode() & 0o777)
                .unwrap_or(0o644)
        };
        temp.as_file()
            .set_permissions(fs::Permissions::from_mode(mode))?;
    }
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}
