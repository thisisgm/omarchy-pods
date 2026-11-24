//! The bench's cache: where each pinned thing lies in it, and the one way it is made.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::pins::{
    self, BUILD_REQUIREMENTS, LIBRARIES, Library, Pin, REQUIREMENTS, SHARED_LIBRARY, Tree,
};
use super::{ToolError, output_of, write_new};
use crate::report;

/// The file setup writes last, which holds the pins the cache was made from.
const READY: &str = "ready";
/// HTTPS only, redirects followed, and a failed transfer is an exit status and one line.
const CURL: [&str; 8] = [
    "--fail",
    "--silent",
    "--show-error",
    "--location",
    "--proto",
    "=https",
    "--proto-redir",
    "=https",
];
const SHA256_HEX_CHARS: usize = 64;
/// Only what is in the requirement file is installed, each file checked against a hash in it.
const PIP: [&str; 8] = [
    "-I",
    "-m",
    "pip",
    "install",
    "--quiet",
    "--disable-pip-version-check",
    "--no-cache-dir",
    "--require-hashes",
];

#[derive(Debug, thiserror::Error)]
pub(crate) enum CacheError {
    #[error("{} exists, and setup makes its cache in a directory of its own; if a setup that failed left it, remove it or name another", .0.display())]
    Exists(PathBuf),
    #[error("could not {what} {}: {source}", .path.display())]
    Io {
        what: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    #[error(transparent)]
    Tool(#[from] ToolError),
    #[error("{file} has the SHA-256 {found}, which is not the one pinned, {expected}")]
    Checksum {
        file: &'static str,
        expected: &'static str,
        found: String,
    },
    #[error("sha256sum printed no digest for {file}")]
    NoDigest { file: &'static str },
    #[error(
        "{} is not a cache made for the pins of this build ({why}); make one with cargo xtask probe bench setup --cache <a directory that does not exist>", .path.display()
    )]
    NotReady { path: PathBuf, why: String },
}

fn io_error(what: &'static str, path: &Path) -> impl FnOnce(io::Error) -> CacheError {
    let path = path.to_owned();
    move |source| CacheError::Io { what, path, source }
}

// Sample input: `a1ada76e...1383ae9e  /cache/downloads/libsbc/sbc-2.2.tar.xz`, with all 64 digits, read as its first field.
fn digest_of(said: &[u8]) -> Option<&str> {
    let digest = str::from_utf8(said).ok()?.split_whitespace().next()?;
    let is_digest =
        digest.len() == SHA256_HEX_CHARS && digest.bytes().all(|byte| byte.is_ascii_hexdigit());
    is_digest.then_some(digest)
}

/// The files of `dir` that end in `extension`, in the order of their names.
fn sources(dir: &Path, extension: &str) -> Result<Vec<PathBuf>, CacheError> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).map_err(io_error("list", dir))? {
        let path = entry.map_err(io_error("list", dir))?.path();
        if path.extension().is_some_and(|ending| ending == extension) {
            found.push(path);
        }
    }
    found.sort();
    Ok(found)
}

#[derive(Debug)]
pub(crate) struct Cache {
    root: PathBuf,
}

impl Cache {
    /// A cache that setup finished for the pins of this build.
    pub(crate) fn open(root: &Path) -> Result<Self, CacheError> {
        let not_ready = |why: String| CacheError::NotReady {
            path: root.to_owned(),
            why,
        };
        let manifest = pins::manifest();
        // One byte past the pins is enough to tell a longer file, so a huge file is never read whole.
        let limit = u64::try_from(manifest.len()).map_or(u64::MAX, |len| len.saturating_add(1));
        let mut listed = Vec::new();
        File::open(root.join(READY))
            .and_then(|file| file.take(limit).read_to_end(&mut listed))
            .map_err(|error| not_ready(format!("its {READY} file was not read: {error}")))?;
        if listed != manifest.as_bytes() {
            return Err(not_ready(format!("its {READY} file lists other pins")));
        }
        Ok(Self {
            root: root.to_owned(),
        })
    }

    pub(crate) fn python(&self) -> PathBuf {
        self.root.join("venv/bin/python")
    }

    pub(crate) fn library(&self, library: &Library) -> PathBuf {
        self.root.join("lib").join(library.file)
    }

    pub(crate) fn download(&self, pin: &Pin) -> PathBuf {
        self.root.join("downloads").join(pin.name).join(pin.file)
    }

    fn make_dir(&self, name: &str) -> Result<PathBuf, CacheError> {
        let dir = self.root.join(name);
        fs::create_dir_all(&dir).map_err(io_error("create", &dir))?;
        Ok(dir)
    }

    /// Downloads one pin into a directory of its own; a file with another SHA-256 than the pinned one stays where it lies and stops the setup.
    fn fetch(&self, pin: &Pin) -> Result<(), CacheError> {
        report(&format!("xtask probe: fetching {}", pin.file));
        let file = self.download(pin);
        self.make_dir(&format!("downloads/{}", pin.name))?;
        let mut curl = Command::new("curl");
        output_of(curl.args(CURL).arg("--output").arg(&file).arg(pin.url))?;
        let said = output_of(Command::new("sha256sum").arg("--").arg(&file))?;
        let found = digest_of(&said).ok_or(CacheError::NoDigest { file: pin.file })?;
        if found != pin.sha256 {
            return Err(CacheError::Checksum {
                file: pin.file,
                expected: pin.sha256,
                found: found.to_owned(),
            });
        }
        Ok(())
    }

    fn build(&self, library: &Library) -> Result<(), CacheError> {
        report(&format!("xtask probe: building {}", library.file));
        let tree = self.make_dir(&format!("src/{}", library.pin.name))?;
        let mut tar = Command::new("tar");
        tar.arg("--extract").arg("--file");
        tar.arg(self.download(&library.pin));
        tar.args(["--strip-components", "1", "--directory"]);
        output_of(tar.arg(&tree))?;
        let (source_dirs, include_dirs): (Vec<PathBuf>, Vec<PathBuf>) = match library.tree {
            Tree::Flat(dir) => (vec![tree.join(dir)], Vec::new()),
            Tree::Parts(parts) => parts
                .iter()
                .map(|part| (tree.join(part).join("src"), tree.join(part).join("include")))
                .unzip(),
        };
        self.make_dir("lib")?;
        let mut compiler = Command::new(library.compiler);
        compiler.args(SHARED_LIBRARY).args(library.flags);
        for dir in include_dirs {
            compiler.arg("-I").arg(dir);
        }
        compiler.arg("-o").arg(self.library(library));
        for dir in source_dirs {
            compiler.args(sources(&dir, library.extension)?);
        }
        output_of(&mut compiler)?;
        Ok(())
    }

    /// The scorer in an environment of its own; the one package that ships as source is built with the build tool the first file pins.
    fn install(&self) -> Result<(), CacheError> {
        report("xtask probe: installing the scorer");
        let venv = self.root.join("venv");
        output_of(Command::new("python3").args(["-I", "-m", "venv"]).arg(venv))?;
        let steps = [
            (
                "build-requirements.txt",
                BUILD_REQUIREMENTS,
                "--only-binary=:all:",
            ),
            ("requirements.txt", REQUIREMENTS, "--no-build-isolation"),
        ];
        for (name, text, how) in steps {
            let file = self.root.join(name);
            write_new(&file, text).map_err(io_error("write", &file))?;
            let mut pip = Command::new(self.python());
            output_of(pip.args(PIP).arg(how).arg("--requirement").arg(&file))?;
        }
        Ok(())
    }
}

/// Makes the cache in `root`, which must not exist, and names it ready only once every step is done.
pub(crate) fn setup(root: &Path) -> Result<(), CacheError> {
    fs::create_dir(root).map_err(|source| {
        if source.kind() == io::ErrorKind::AlreadyExists {
            CacheError::Exists(root.to_owned())
        } else {
            io_error("create", root)(source)
        }
    })?;
    let cache = Cache {
        root: root.to_owned(),
    };
    for pin in pins::downloads() {
        cache.fetch(&pin)?;
    }
    for library in &LIBRARIES {
        cache.build(library)?;
    }
    cache.install()?;
    let ready = root.join(READY);
    write_new(&ready, &pins::manifest()).map_err(io_error("write", &ready))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::pins::{self, SBC};
    use super::{Cache, READY, digest_of};

    /// What `Cache::open` says of a root whose ready file holds `contents`: "opened", or the refusal.
    fn opened_with_ready(contents: impl AsRef<[u8]>) -> String {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join(READY), contents).unwrap();
        Cache::open(root.path()).map_or_else(|refused| refused.to_string(), |_| "opened".to_owned())
    }

    #[test]
    fn a_ready_file_of_this_build_opens() {
        assert_eq!(opened_with_ready(pins::manifest()), "opened");
    }

    #[test]
    fn a_ready_file_longer_than_the_pins_is_other_pins() {
        let refused = opened_with_ready(format!("{}{}", pins::manifest(), "x".repeat(4096)));
        assert!(refused.contains("lists other pins"), "{refused}");
    }

    // corner: Linux only; a ready file with no end, read whole, would never return.
    #[test]
    fn the_ready_file_is_read_no_further_than_the_pins() {
        let root = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/dev/zero", root.path().join(READY)).unwrap();
        let refused = Cache::open(root.path()).err().unwrap();
        assert!(
            refused.to_string().contains("lists other pins"),
            "{refused}"
        );
    }

    #[test]
    fn a_digest_is_the_first_field_and_sixty_four_hex_digits() {
        let line = format!("{}  /cache/downloads/libsbc/{}\n", SBC.sha256, SBC.file);
        assert_eq!(digest_of(line.as_bytes()), Some(SBC.sha256));
        let short = SBC.sha256.strip_prefix('a').unwrap();
        let no_digits = "g".repeat(SBC.sha256.len());
        for not_one in [
            "",
            "\n",
            "sha256sum: no such file",
            short,
            "zz  file",
            &no_digits,
        ] {
            assert_eq!(digest_of(not_one.as_bytes()), None, "{not_one:?}");
        }
        assert_eq!(digest_of(&[0xff; 64]), None);
    }
}
