//! Local-socket naming for the daemon control channel. The one place the
//! platform socket form is chosen, so the rest of the daemon stays
//! transport-agnostic.

use interprocess::local_socket::Name;
use std::path::Path;

/// Build the local-socket name for the daemon endpoint backed by `path`.
///
/// Unix: a filesystem-path socket at `path`. Windows: a named pipe whose name
/// is derived from the full `path`, so distinct state dirs (e.g. isolated test
/// homes) map to distinct pipes.
#[cfg(unix)]
pub fn socket_name(path: &Path) -> std::io::Result<Name<'static>> {
    use interprocess::local_socket::{GenericFilePath, ToFsName};
    path.to_path_buf().to_fs_name::<GenericFilePath>()
}

#[cfg(windows)]
pub fn socket_name(path: &Path) -> std::io::Result<Name<'static>> {
    use interprocess::local_socket::{GenericNamespaced, ToNsName};
    // Named pipes live in their own namespace, not the filesystem; fold the
    // full path into one collision-free pipe name.
    let sanitized: String = path
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("devkit-portd-{sanitized}.sock").to_ns_name::<GenericNamespaced>()
}
