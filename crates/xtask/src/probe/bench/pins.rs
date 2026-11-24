//! What the bench's cache is built from: every download with its SHA-256, the compiler call of each library, and the Python packages with theirs.

/// One file the cache is built from, taken only when its SHA-256 is the one here.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Pin {
    /// The directory of the download in the cache, and the key of its row.
    pub(crate) name: &'static str,
    pub(crate) file: &'static str,
    pub(crate) url: &'static str,
    pub(crate) sha256: &'static str,
}

/// libsbc, the encoder `PipeWire`'s SBC codecs call.
pub(crate) const SBC: Pin = Pin {
    name: "libsbc",
    file: "sbc-2.2.tar.xz",
    url: "https://www.kernel.org/pub/linux/bluetooth/sbc-2.2.tar.xz",
    sha256: "a1ada76ef35e5af9c2fbd063754dc9e37a8d989417c6eb1ecebb089b1383ae9e",
};

/// libfdk-aac, the encoder `PipeWire`'s AAC codec calls.
pub(crate) const FDK_AAC: Pin = Pin {
    name: "libfdk-aac",
    file: "fdk-aac-2.0.3.tar.gz",
    url: "https://downloads.sourceforge.net/project/opencore-amr/fdk-aac/fdk-aac-2.0.3.tar.gz",
    sha256: "829b6b89eef382409cda6857fd82af84fabb63417b08ede9ea7a553f811cb79e",
};

/// Where a source archive keeps what one library is compiled from.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Tree {
    /// The sources of one directory, with their headers beside them.
    Flat(&'static str),
    /// The sources in `src` of each part, and the headers in `include` of each.
    Parts(&'static [&'static str]),
}

/// One codec library and the single compiler call that builds it, so that nothing an archive holds is ever run.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Library {
    pub(crate) pin: Pin,
    pub(crate) file: &'static str,
    pub(crate) compiler: &'static str,
    pub(crate) extension: &'static str,
    /// The flags the library's own build sets that change the code.
    pub(crate) flags: &'static [&'static str],
    pub(crate) tree: Tree,
}

/// The flags every library is compiled with.
pub(crate) const SHARED_LIBRARY: [&str; 3] = ["-shared", "-fPIC", "-O2"];

pub(crate) const LIBSBC: Library = Library {
    pin: SBC,
    file: "libsbc.so",
    compiler: "cc",
    extension: "c",
    flags: &[],
    tree: Tree::Flat("sbc"),
};

pub(crate) const LIBFDK_AAC: Library = Library {
    pin: FDK_AAC,
    file: "libfdk-aac.so",
    compiler: "c++",
    extension: "cpp",
    flags: &["-fno-exceptions", "-fno-rtti"],
    tree: Tree::Parts(&[
        "libAACdec",
        "libAACenc",
        "libArithCoding",
        "libDRCdec",
        "libFDK",
        "libMpegTPDec",
        "libMpegTPEnc",
        "libPCMutils",
        "libSACdec",
        "libSACenc",
        "libSBRdec",
        "libSBRenc",
        "libSYS",
    ]),
};

/// The libraries in the order the cache builds them.
pub(crate) const LIBRARIES: [Library; 2] = [LIBSBC, LIBFDK_AAC];

impl Library {
    /// Everything the compiler call is made from, as one line of the manifest.
    fn recipe(&self) -> String {
        let tree = match self.tree {
            Tree::Flat(dir) => format!("flat {dir}"),
            Tree::Parts(parts) => format!("parts {}", parts.join(" ")),
        };
        let flags = [SHARED_LIBRARY.join(" "), self.flags.join(" ")];
        let call = [
            self.file,
            self.compiler,
            self.extension,
            &flags[0],
            &flags[1],
            &tree,
        ];
        format!("{}\n", call.join("\t"))
    }
}

/// One piece of reference music and where its excerpt starts.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Track {
    pub(crate) pin: Pin,
    pub(crate) start_s: u32,
}

/// How long every excerpt is.
pub(crate) const EXCERPT_S: u32 = 20;

/// Lossless stereo recordings on Wikimedia Commons: two under CC BY-SA 4.0 by Claude Reid, one in the public domain, one under CC BY-SA 4.0 by Richard D Siegel.
pub(crate) const TRACKS: [Track; 4] = [
    Track {
        pin: Pin {
            name: "funk",
            file: "Jazz_Funk_no1_(accompagnement).flac",
            url: "https://upload.wikimedia.org/wikipedia/commons/c/c2/Jazz_Funk_no1_%28accompagnement%29.flac",
            sha256: "92002ab7fd547ab3677a1db377e79840f5eb80fc8bf3537e2864397f817b2fb8",
        },
        start_s: 10,
    },
    Track {
        pin: Pin {
            name: "piano",
            file: "Kimiko_Ishizaka_-_Bach-_Well-Tempered_Clavier,_Book_1_-_29_Prelude_No._15_in_G_major,_BWV_860.flac",
            url: "https://upload.wikimedia.org/wikipedia/commons/3/32/Kimiko_Ishizaka_-_Bach-_Well-Tempered_Clavier%2C_Book_1_-_29_Prelude_No._15_in_G_major%2C_BWV_860.flac",
            sha256: "58703d9122ef674ea3e9acda2834463a1011e84746cf82fa5bd62f5f67fdc924",
        },
        start_s: 5,
    },
    Track {
        pin: Pin {
            name: "coucou",
            file: "Louis-Claude_Daquin_Le_Coucou.flac",
            url: "https://upload.wikimedia.org/wikipedia/commons/a/a4/Louis-Claude_Daquin_Le_Coucou.flac",
            sha256: "41fb308da5eccd43d189e71d3e0beb0e2de66c2333a9de29453c377f5e218f4b",
        },
        start_s: 10,
    },
    Track {
        pin: Pin {
            name: "poprock",
            file: "Pop_RockBrit_(accompagnement).flac",
            url: "https://upload.wikimedia.org/wikipedia/commons/c/c9/Pop_RockBrit_%28accompagnement%29.flac",
            sha256: "224776e1d5355924d31154e3d898284265e8de9d866b87664518e6acc1a17265",
        },
        start_s: 30,
    },
];

/// What builds the one package that ships as source, installed first so that build reads nothing unpinned.
pub(crate) const BUILD_REQUIREMENTS: &str = include_str!("build-requirements.txt");
/// The scorer and what it imports, each with the hashes of its files for `CPython` 3.13 and 3.14 on x86-64.
pub(crate) const REQUIREMENTS: &str = include_str!("requirements.txt");

/// Every download in the order the cache takes them.
pub(crate) fn downloads() -> impl Iterator<Item = Pin> {
    let sources = LIBRARIES.into_iter().map(|library| library.pin);
    sources.chain(TRACKS.into_iter().map(|track| track.pin))
}

/// What a finished cache holds as its last file, so a cache made from other pins or by another compiler call is not read as this one.
pub(crate) fn manifest() -> String {
    let files = downloads().map(|pin| format!("{}\t{}\t{}\n", pin.name, pin.file, pin.sha256));
    let recipes = LIBRARIES.iter().map(Library::recipe);
    files
        .chain(recipes)
        .chain([BUILD_REQUIREMENTS.to_owned(), REQUIREMENTS.to_owned()])
        .collect()
}
