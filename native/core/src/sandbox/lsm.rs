//! Landlock file (and TCP) restrictions for shell commands that run without
//! bubblewrap. Landlock forbids the mount calls bubblewrap needs, so it cannot
//! be stacked under bubblewrap without a helper inside the sandbox; it is the
//! layer that still applies when bubblewrap is missing or broken.
//!
//! The ruleset is built here, in the parent; the forked child only calls
//! `prctl(PR_SET_NO_NEW_PRIVS)` and `landlock_restrict_self` on its fd.
use anyhow::Result;
use landlock::{
    Access, AccessFs, AccessNet, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset,
    RulesetAttr, RulesetCreatedAttr, Scope, ABI,
};
use std::{
    io,
    os::fd::{AsRawFd, OwnedFd},
    path::{Path, PathBuf},
};

const ABI_WANTED: ABI = ABI::V5;
/// Newest ABI whose extra rights (V6 abstract-socket/signal scopes, V9
/// `ResolveUnix` for named sockets) harden the fallback. `CompatLevel::
/// BestEffort` drops what an older kernel does not support.
const ABI_SCOPED: ABI = ABI::V9;

/// Highest Landlock ABI the running kernel supports (0 when unavailable).
pub fn kernel_abi() -> i64 {
    // SAFETY: the documented version query takes a null attribute pointer.
    let version = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0usize,
            1u32, // LANDLOCK_CREATE_RULESET_VERSION
        )
    };
    version.max(0)
}

pub struct Paths<'a> {
    pub read_only: Vec<PathBuf>,
    pub writable: Vec<PathBuf>,
    pub workspace: &'a Path,
    pub deny_tcp: bool,
}

/// Build a ruleset: read/execute below the system roots and the allowed home
/// entries, full access below the workspace and temporary directories,
/// nothing anywhere else. Returns `None` when the kernel has no Landlock.
pub fn ruleset(paths: &Paths) -> Result<Option<OwnedFd>> {
    if kernel_abi() == 0 {
        return Ok(None);
    }
    let read = AccessFs::from_read(ABI_WANTED);
    // Connecting to a named Unix socket outside the workspace (the D-Bus
    // session bus, ShadowCode's own control socket) is governed by
    // `ResolveUnix` (ABI 9); grant it only where files are writable.
    let all = AccessFs::from_all(ABI_WANTED) | AccessFs::ResolveUnix;
    let mut base = Ruleset::default()
        .set_compatibility(CompatLevel::BestEffort)
        .handle_access(all)?
        // Abstract Unix sockets (the X11 socket, an abstract D-Bus bus) and
        // signals to processes outside the sandbox (ABI 6+).
        .scope(Scope::from_all(ABI_SCOPED))?;
    if paths.deny_tcp {
        base = base.handle_access(AccessNet::from_all(ABI_WANTED))?;
    }
    let mut created = base.create()?;
    for path in &paths.read_only {
        if let Ok(fd) = PathFd::new(path) {
            created = created.add_rule(PathBeneath::new(fd, read))?;
        }
    }
    let mut writable = paths.writable.clone();
    writable.push(paths.workspace.to_path_buf());
    for path in &writable {
        if let Ok(fd) = PathFd::new(path) {
            created = created.add_rule(PathBeneath::new(fd, all))?;
        }
    }
    Ok(created.into())
}

/// Runs in the forked child before exec.
///
/// # Safety
/// Call only between fork and exec.
pub unsafe fn restrict(fd: &OwnedFd) -> io::Result<()> {
    if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
        return Err(io::Error::last_os_error());
    }
    if libc::syscall(libc::SYS_landlock_restrict_self, fd.as_raw_fd(), 0u32) != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
