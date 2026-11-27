//! The C++ daemon's `QSettings` file, read at most once when startup needs it.

use std::cell::OnceCell;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use zeroize::Zeroizing;

use super::{log, owner_file, state_file};

const HOME_CONFIG_DIR: &str = ".config";
const OLD_DIR_NAME: &str = "AirPodsTrayApp";
const OLD_FILE_NAME: &str = "AirPodsTrayApp.conf";

// Sample input: `XDG_CONFIG_HOME=/home/u/.config`, or `HOME=/home/u` if unset, empty or relative.
pub(crate) fn path(xdg_config_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    Some(
        state_file::xdg_dir(xdg_config_home, home, HOME_CONFIG_DIR)?
            .join(OLD_DIR_NAME)
            .join(OLD_FILE_NAME),
    )
}

pub(super) struct LegacyConf<'a> {
    pub(super) path: Option<&'a Path>,
    bytes: OnceCell<Option<Zeroizing<Vec<u8>>>>,
}

impl<'a> LegacyConf<'a> {
    pub(super) const fn new(path: Option<&'a Path>) -> Self {
        Self {
            path,
            bytes: OnceCell::new(),
        }
    }

    pub(super) fn read(&self) -> Option<&[u8]> {
        let path = self.path?;
        self.bytes
            .get_or_init(|| match owner_file::read(path) {
                Ok(bytes) => bytes.map(Zeroizing::new),
                Err(error) => {
                    log(&format!(
                        "legacy config: cannot read {}: {error}",
                        path.display()
                    ));
                    None
                }
            })
            .as_ref()
            .map(|bytes| bytes.as_slice())
    }
}
