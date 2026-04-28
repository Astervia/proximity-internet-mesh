//! Cross-platform runtime path resolution for the daemon's transient
//! files (Unix socket, PID, stats, debug snapshot).
//!
//! Linux uses `/run/pim/*`, the FHS-canonical location for runtime data.
//! Production deployments run as the `pim` user/group via a systemd unit
//! that owns the directory.
//!
//! macOS has no `/run`. We use `$TMPDIR/*` — the per-user tmpdir launchd
//! allocates per the docs/RPC.md §1.2 macOS row. All four files
//! (sock + pid + stats + debug) live alongside each other under one root.
//!
//! Both platforms honour `$PIM_RPC_SOCKET` for the socket path so users
//! can override at run time without recompiling.

use std::path::PathBuf;

/// Per-platform "runtime" root directory for the daemon's transient files.
///
/// Linux uses hard-coded `/run/...` paths in every caller for FHS / ABI
/// reasons, so `runtime_dir` is only needed on non-Linux targets.
#[cfg(not(target_os = "linux"))]
fn runtime_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        PathBuf::from("/tmp")
    }
}

/// Stats file (`/run/pim/pim.stats` on Linux, `$TMPDIR/pim.stats` on macOS).
pub(crate) fn stats_path() -> PathBuf {
    // Historical: pim.stats was in `/run/` (not `/run/pim/`) on Linux. We
    // keep that ABI on Linux to avoid breaking `pim status --verbose`
    // readers that hardcoded the old path.
    #[cfg(target_os = "linux")]
    {
        PathBuf::from("/run/pim.stats")
    }
    #[cfg(not(target_os = "linux"))]
    {
        runtime_dir().join("pim.stats")
    }
}

/// Debug snapshot file used by `pim debug`.
pub(crate) fn debug_snapshot_path() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        PathBuf::from("/run/pim-debug.json")
    }
    #[cfg(not(target_os = "linux"))]
    {
        runtime_dir().join("pim-debug.json")
    }
}

/// JSON-RPC Unix-socket path. Honours `$PIM_RPC_SOCKET` override per
/// docs/RPC.md §1.2.
pub(crate) fn rpc_socket_path() -> PathBuf {
    if let Ok(p) = std::env::var("PIM_RPC_SOCKET") {
        return PathBuf::from(p);
    }
    #[cfg(target_os = "linux")]
    {
        // Try user runtime dir first ($XDG_RUNTIME_DIR/pim.sock), fall
        // back to the system-daemon location. The user-dir path matches
        // pim-ui's resolver (socket_path.rs in pim-ui).
        if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR") {
            return PathBuf::from(xdg).join("pim.sock");
        }
        PathBuf::from("/run/pim/pim.sock")
    }
    #[cfg(not(target_os = "linux"))]
    {
        runtime_dir().join("pim.sock")
    }
}

/// Default PID-file path when `pim-daemon` is started without an explicit
/// argv override. pim-ui passes its own path explicitly so this only
/// matters for the standalone CLI.
pub(crate) fn default_pid_file() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        PathBuf::from("/run/pim.pid")
    }
    #[cfg(not(target_os = "linux"))]
    {
        runtime_dir().join("pim.pid")
    }
}

/// Default config-file path when `pim-daemon` is started without an
/// explicit argv override.
pub(crate) fn default_config_path() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        PathBuf::from("/etc/pim/pim.toml")
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            PathBuf::from(home).join("Library/Application Support/pim/pim.toml")
        } else {
            PathBuf::from("/etc/pim/pim.toml")
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        PathBuf::from("./pim.toml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_socket_path_honors_env_override() {
        let prev = std::env::var("PIM_RPC_SOCKET").ok();
        std::env::set_var("PIM_RPC_SOCKET", "/tmp/test-pim.sock");
        assert_eq!(rpc_socket_path(), PathBuf::from("/tmp/test-pim.sock"));
        match prev {
            Some(v) => std::env::set_var("PIM_RPC_SOCKET", v),
            None => std::env::remove_var("PIM_RPC_SOCKET"),
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stats_path_keeps_legacy_linux_location() {
        assert_eq!(stats_path(), PathBuf::from("/run/pim.stats"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn stats_path_uses_tmpdir_on_macos() {
        let p = stats_path();
        assert!(
            p.to_string_lossy().ends_with("/pim.stats"),
            "expected pim.stats suffix, got {p:?}"
        );
    }
}
