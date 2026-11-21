//! Where the daemon socket lives, as `daemon/ipcpath.hpp` decides it.

use std::ffi::OsStr;
use std::path::PathBuf;

/// Appended to the runtime directory as text, separator included, exactly as the C++ does.
const SOCKET_SUFFIX: &str = "/librepods.sock";

pub(crate) fn socket_path(xdg_runtime_dir: Option<&OsStr>) -> Option<PathBuf> {
    // Callers fail rather than fall back, or the socket lands in a world-visible directory.
    let dir = xdg_runtime_dir.filter(|dir| !dir.is_empty())?;
    let mut path = dir.to_os_string();
    path.push(SOCKET_SUFFIX);
    Some(PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;

    use super::socket_path;

    #[test]
    fn unset_runtime_dir_has_no_socket() {
        assert_eq!(socket_path(None), None);
    }

    #[test]
    fn empty_runtime_dir_has_no_socket() {
        assert_eq!(socket_path(Some(OsStr::new(""))), None);
    }

    #[test]
    fn set_runtime_dir_gives_an_absolute_path_under_it() {
        let path = socket_path(Some(OsStr::new("/run/user/1000"))).unwrap();
        assert_eq!(path, Path::new("/run/user/1000/librepods.sock"));
        assert!(path.is_absolute());
    }

    #[test]
    fn trailing_slash_is_kept_as_the_cpp_keeps_it() {
        let path = socket_path(Some(OsStr::new("/run/user/1000/"))).unwrap();
        assert_eq!(
            path.as_os_str(),
            OsStr::new("/run/user/1000//librepods.sock")
        );
    }
}
