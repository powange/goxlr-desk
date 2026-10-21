//! Where the daemon's IPC socket lives, shared by the daemon, the client and the launcher.

use std::path::PathBuf;

/// The named pipe used on Windows.
pub const NAMED_PIPE: &str = "@goxlr.socket";

/// Where earlier versions put the socket, shared by every user of the machine.
pub const LEGACY_SOCKET_PATH: &str = "/tmp/goxlr.socket";

/// The socket path for this user: in $XDG_RUNTIME_DIR (private to the user), in the per-user
/// $TMPDIR on macOS, otherwise the legacy shared path.
pub fn socket_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute() && dir.is_dir())
    {
        return dir.join("goxlr.socket");
    }
    if cfg!(target_os = "macos")
        && let Some(dir) = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute() && dir.is_dir())
    {
        return dir.join("goxlr.socket");
    }
    PathBuf::from(LEGACY_SOCKET_PATH)
}

/// Where a client looks for the daemon: this user's socket first, then the legacy path (a
/// daemon started without the user's session environment).
pub fn socket_candidates() -> Vec<PathBuf> {
    let mut paths = vec![socket_path()];
    let legacy = PathBuf::from(LEGACY_SOCKET_PATH);
    if !paths.contains(&legacy) {
        paths.push(legacy);
    }
    paths
}
