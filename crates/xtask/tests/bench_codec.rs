//! The built binary's `probe bench` commands, with shell scripts standing in for the pinned tools of the cache and for the downloader.

#[cfg(test)]
#[path = "../benches/codec/check.rs"]
mod check;
#[cfg(test)]
#[path = "../src/probe/bench/pins.rs"]
mod pins;

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod tests {
    use std::env;
    use std::fs::{self, Permissions};
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    use tempfile::TempDir;

    use super::check::{check, when_asked};
    use super::pins::{EXCERPT_S, SBC, TRACKS, manifest};

    /// The status the command exits with when it refuses; a panic would exit with 101.
    const REFUSED: i32 = 1;
    const RUNNABLE: u32 = 0o700;
    const RUNS: usize = 2;
    /// The two digits the scorer's figures count down from.
    const CEILING: usize = 99;
    /// What the scorer prints when it dies inside Python.
    const TRACEBACK: &str = "echo 'Traceback (most recent call last):'";
    /// The SHA-256 of `not the file` and a newline, which the false curl writes.
    const FALSE_DOWNLOAD_SHA256: &str =
        "c439c413d8dfc696bbb4f856625528280cb368a8ff5285e9ee427b5d8984569e";
    const SBC_XQ_SETTINGS: &str =
        "frequency=3 blocks=3 subbands=1 mode=1 allocation=0 bitpool=39 endian=0";
    const AAC_SETTINGS: &str = "0x0100=129 0x0103=48000 0x0106=2 0x0102=5 0x0304=2 0x0302=1 0x0101=320000 0x0207=376875 0x0300=6 0x0301=1 0x0200=1";

    /// The scorer's lines for its `count`th call: the figures fall with every call, so the least of a row is its last run and not its first.
    fn scores() -> String {
        format!(
            r#"printf 'MOS-LQO:      4.%02d0000\nVNSIM:        0.9%02d000\n' "$(({CEILING} - count))" "$(({CEILING} - count))""#
        )
    }

    /// A cache as `setup` leaves it, whose interpreter is a script that logs each call into `calls.log` and scores with `scorer`.
    fn cache_with(scorer: &str) -> TempDir {
        let cache = tempfile::tempdir().unwrap();
        fs::write(cache.path().join("ready"), manifest()).unwrap();
        let bin = cache.path().join("venv/bin");
        fs::create_dir_all(&bin).unwrap();
        let script = format!(
            r#"#!/bin/sh
log="$(dirname "$(dirname "$(dirname "$0")")")/calls.log"
if [ "$2" = -m ]; then
    printf 'visqol %s %s\n' "$(basename "$5")" "$(basename "$7")" >> "$log"
    count=$(grep -c '^visqol ' "$log")
    {scorer}
    exit
fi
verb=$4
shift 4
case "$verb" in
versions) printf 'python\t3.13.5\nvisqol-python\t3.8.0\n' ;;
excerpt) printf 'excerpt %s %s %s %s\n' "$(basename "$1")" "$2" "$3" "$4" >> "$log" ;;
*)
    library=$(basename "$1") heard=$(basename "$2")
    shift 3
    printf '%s %s %s %s\n' "$verb" "$library" "$heard" "$*" >> "$log"
    ;;
esac
"#
        );
        let python = bin.join("python");
        fs::write(&python, script).unwrap();
        fs::set_permissions(&python, Permissions::from_mode(RUNNABLE)).unwrap();
        cache
    }

    fn bench(args: &[&str], cache: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
        command.args(["probe", "bench"]).args(args);
        command.arg("--cache").arg(cache).env("HOME", cache);
        command
    }

    fn codec(cache: &Path, out: &Path) -> Output {
        let mut command = bench(&["codec", "--runs", &RUNS.to_string()], cache);
        command.arg("--out").arg(out).output().unwrap()
    }

    fn error_of(output: &Output) -> String {
        String::from_utf8(output.stderr.clone()).unwrap()
    }

    /// The value and the note of the one row with `key`.
    fn row<'a>(table: &'a str, key: &str) -> (&'a str, &'a str) {
        let mut found = table.lines().filter_map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            (fields[0] == key).then(|| (fields[1], fields[3]))
        });
        let row = found.next().unwrap_or_else(|| panic!("no row {key}"));
        assert_eq!(found.next(), None, "{key} twice");
        row
    }

    #[test]
    fn codec_scores_every_track_through_both_codecs_and_writes_ranges() {
        let cache = cache_with(&scores());
        let out = cache.path().join("codec.tsv");
        let scored = codec(cache.path(), &out);
        assert_eq!(error_of(&scored), "");
        assert!(scored.status.success(), "{scored:?}");
        let table = String::from_utf8(scored.stdout).unwrap();
        assert_eq!(fs::read_to_string(&out).unwrap(), table);
        assert!(table.starts_with("key\tvalue\tunit\tnote\n"), "{table}");

        assert_eq!(row(&table, "tool_python"), ("3.13.5", "in the cache"));
        assert_eq!(row(&table, "tool_visqol-python").0, "3.8.0");
        let (file, note) = row(&table, "pin_libsbc");
        assert_eq!((file, note), (SBC.file, &*format!("sha256 {}", SBC.sha256)));
        for (key, settings, sink) in [
            (
                "settings_sbc_xq",
                SBC_XQ_SETTINGS,
                "a bitpool of 39 or more",
            ),
            ("settings_aac", AAC_SETTINGS, "packets of 1017 bytes"),
        ] {
            let (value, note) = row(&table, key);
            assert_eq!(value, settings);
            assert!(note.contains(sink), "{note}");
            assert!(
                note.ends_with("; assumed, not read from the pods"),
                "{note}"
            );
        }

        let log = fs::read_to_string(cache.path().join("calls.log")).unwrap();
        let runs = format!("{RUNS} runs");
        let mut call = 0;
        for track in TRACKS {
            let name = track.pin.name;
            let (file, note) = row(&table, &format!("track_{name}"));
            assert_eq!(file, track.pin.file);
            let cut = format!("{EXCERPT_S} s from {} s at 48000 Hz", track.start_s);
            assert_eq!(note, format!("sha256 {}; {cut}", track.pin.sha256));
            let excerpt = format!(
                "excerpt {} {} {EXCERPT_S} 48000\n",
                track.pin.file, track.start_s
            );
            assert!(log.contains(&excerpt), "{log}");
            for codec in ["reference", "sbc_xq", "aac"] {
                let (first, last) = (CEILING - call - 1, CEILING - call - RUNS);
                call += RUNS;
                let mos = row(&table, &format!("visqol_mos_{codec}_{name}"));
                let wanted = format!("4.{last:02}0000 to 4.{first:02}0000");
                assert_eq!(mos, (&*wanted, &*runs), "{codec} {name}");
                let vnsim = row(&table, &format!("visqol_vnsim_{codec}_{name}"));
                let wanted = format!("0.9{last:02}000 to 0.9{first:02}000");
                assert_eq!(vnsim, (&*wanted, &*runs), "{codec} {name}");
            }
            let passes = [
                format!("sbc libsbc.so {name}.wav {SBC_XQ_SETTINGS}\n"),
                format!("aac libfdk-aac.so {name}.wav {AAC_SETTINGS}\n"),
                format!("visqol {name}.wav {name}.wav\n"),
                format!("visqol {name}.wav {name}.sbc_xq.wav\n"),
                format!("visqol {name}.wav {name}.aac.wav\n"),
            ];
            for pass in passes {
                assert_eq!(log.matches(&pass).count(), RUNS, "{pass}{log}");
            }
        }
        assert_eq!(table.lines().count(), 1 + 2 + 2 + 2 + TRACKS.len() * 7);
    }

    #[test]
    fn codec_refuses_a_cache_that_setup_did_not_finish_for_these_pins() {
        let other_flags = manifest().replace("-fno-rtti", "-frtti");
        assert_ne!(other_flags, manifest());
        let older_pin = "libsbc\tsbc-2.1.tar.xz\tan older pin\n";
        for ready in [None, Some(older_pin), Some(&*other_flags)] {
            let cache = cache_with(&scores());
            match ready {
                None => fs::remove_file(cache.path().join("ready")).unwrap(),
                Some(other) => fs::write(cache.path().join("ready"), other).unwrap(),
            }
            let out = cache.path().join("codec.tsv");
            let refused = codec(cache.path(), &out);
            assert_eq!(refused.status.code(), Some(REFUSED), "{refused:?}");
            assert_eq!(refused.stdout, b"");
            let error = error_of(&refused);
            assert!(error.contains("cargo xtask probe bench setup"), "{error}");
            assert!(!out.exists());
            assert!(!cache.path().join("calls.log").exists());
        }
    }

    #[test]
    fn a_scorer_that_gives_no_score_is_a_refusal_that_says_what_it_printed() {
        let cache = cache_with(TRACEBACK);
        let out = cache.path().join("codec.tsv");
        let refused = codec(cache.path(), &out);
        assert_eq!(refused.status.code(), Some(REFUSED), "{refused:?}");
        assert_eq!(refused.stdout, b"");
        let error = error_of(&refused);
        let named = "/funk.wav: the scorer printed no MOS-LQO: line";
        assert!(error.contains(named), "{error}");
        assert!(
            error.contains("Traceback (most recent call last):"),
            "{error}"
        );
        assert!(!out.exists());
    }

    #[test]
    fn a_scorer_that_fails_is_named_with_its_status_and_no_table_is_kept() {
        let cache = cache_with("echo broken >&2; exit 3");
        let out = cache.path().join("codec.tsv");
        let refused = codec(cache.path(), &out);
        assert_eq!(refused.status.code(), Some(REFUSED), "{refused:?}");
        let error = error_of(&refused);
        assert!(error.starts_with("broken\n"), "{error}");
        assert!(error.contains("venv/bin/python"), "{error}");
        assert!(error.contains("exit status: 3"), "{error}");
        assert!(!out.exists());
    }

    #[test]
    fn codec_never_writes_over_a_table() {
        let cache = cache_with(&scores());
        let out = cache.path().join("codec.tsv");
        fs::write(&out, "kept").unwrap();
        let refused = codec(cache.path(), &out);
        assert_eq!(refused.status.code(), Some(REFUSED), "{refused:?}");
        assert!(error_of(&refused).contains("codec.tsv"), "{refused:?}");
        assert_eq!(fs::read_to_string(&out).unwrap(), "kept");
        assert!(!cache.path().join("calls.log").exists());
    }

    /// A directory ahead of the path that holds a `curl` which logs its address and writes one line where the download belongs.
    fn path_with_a_false_curl(log: &Path) -> (TempDir, std::ffi::OsString) {
        let dir = tempfile::tempdir().unwrap();
        let script = format!(
            "#!/bin/sh\nfor last; do :; done\necho \"$last\" >> {}\nwhile [ \"$1\" != --output ]; do shift; done\necho 'not the file' > \"$2\"\n",
            log.display()
        );
        let curl = dir.path().join("curl");
        fs::write(&curl, script).unwrap();
        fs::set_permissions(&curl, Permissions::from_mode(RUNNABLE)).unwrap();
        let rest = env::var_os("PATH").unwrap();
        let ahead = [PathBuf::from(dir.path())];
        let path = env::join_paths(ahead.into_iter().chain(env::split_paths(&rest))).unwrap();
        (dir, path)
    }

    #[test]
    fn setup_refuses_a_download_with_another_checksum_and_builds_nothing_from_it() {
        let parent = tempfile::tempdir().unwrap();
        let cache = parent.path().join("cache");
        let log = parent.path().join("fetched.log");
        let (_curl, path) = path_with_a_false_curl(&log);
        let refused = bench(&["setup"], &cache)
            .env("PATH", path)
            .output()
            .unwrap();
        assert_eq!(refused.status.code(), Some(REFUSED), "{refused:?}");
        let error = error_of(&refused);
        assert!(error.contains(SBC.file), "{error}");
        assert!(error.contains(SBC.sha256), "{error}");
        assert!(error.contains(FALSE_DOWNLOAD_SHA256), "{error}");
        assert_eq!(fs::read_to_string(&log).unwrap(), format!("{}\n", SBC.url));
        let left: Vec<_> = fs::read_dir(&cache)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left, ["downloads"]);
        let again = bench(&["setup"], &cache).output().unwrap();
        assert_eq!(again.status.code(), Some(REFUSED), "{again:?}");
        let error = error_of(&again);
        assert!(error.contains("a setup that failed"), "{error}");
    }

    #[test]
    fn setup_refuses_a_directory_that_exists() {
        let cache = tempfile::tempdir().unwrap();
        let log = cache.path().join("fetched.log");
        let (_curl, path) = path_with_a_false_curl(&log);
        let refused = bench(&["setup"], cache.path())
            .env("PATH", path)
            .output()
            .unwrap();
        assert_eq!(refused.status.code(), Some(REFUSED), "{refused:?}");
        assert!(error_of(&refused).contains("exists"), "{refused:?}");
        assert_eq!(fs::read_dir(cache.path()).unwrap().count(), 0);
    }

    #[test]
    fn the_check_of_the_bench_reads_the_table_the_built_codec_writes() {
        for (mos, passes) in [("4.700000", true), ("4.400000", false)] {
            let lines = format!("printf 'MOS-LQO:      {mos}\\nVNSIM:        0.990000\\n'");
            let cache = cache_with(&lines);
            let scored = codec(cache.path(), &cache.path().join("codec.tsv"));
            assert!(scored.status.success(), "{scored:?}");
            let checked = check(&String::from_utf8(scored.stdout).unwrap());
            assert_eq!(checked.is_ok(), passes, "{mos}: {checked:?}");
        }
    }

    #[test]
    fn the_bench_does_its_work_only_when_cargo_asks_for_a_bench() {
        let started = |args: &[&str]| {
            let mut worked = false;
            let work = || {
                worked = true;
                Ok(())
            };
            let did = when_asked(args.iter().map(Into::into), work);
            (did, worked)
        };
        let program = "codec-0123456789abcdef";
        assert_eq!(started(&[program, "--bench"]), (Ok(true), true));
        assert_eq!(started(&[program]), (Ok(false), false));
        assert_eq!(started(&[program, "--list"]), (Ok(false), false));
        let failed = when_asked(
            ["--bench".into()].into_iter(),
            || Err("no table".to_owned()),
        );
        assert_eq!(failed, Err("no table".to_owned()));
    }

    /// A table of one track as the bench reads it, with the three ranges given.
    fn table_of(reference: &str, sbc_xq: &str, aac: &str) -> String {
        format!(
            "key\tvalue\tunit\tnote\nvisqol_mos_reference_funk\t{reference}\tMOS-LQO\t2 runs\nvisqol_mos_sbc_xq_funk\t{sbc_xq}\tMOS-LQO\t2 runs\nvisqol_mos_aac_funk\t{aac}\tMOS-LQO\t2 runs\n"
        )
    }

    #[test]
    fn the_bench_passes_only_a_table_with_every_codec_between_the_floor_and_its_reference() {
        let ceiling = "4.732101 to 4.732101";
        assert_eq!(
            check(&table_of(ceiling, "4.731460 to 4.731460", "4.5 to 4.6")),
            Ok(())
        );
        let refused = [
            table_of(ceiling, "4.731460 to 4.732102", "4.6 to 4.6"),
            table_of(ceiling, "4.731460 to 4.731460", "4.499999 to 4.6"),
            table_of(ceiling, "4.7 to 4.6", "4.6 to 4.6"),
            table_of(ceiling, "4.7 to NaN", "4.6 to 4.6"),
            table_of(ceiling, "4.7", "4.6 to 4.6"),
            table_of(ceiling, "4.7 to 4.7", "4.6 to 4.6")
                .replace("visqol_mos_aac", "visqol_mos_opus"),
            "key\tvalue\tunit\tnote\n".to_owned(),
        ];
        for table in refused {
            assert!(check(&table).is_err(), "{table}");
        }
    }
}
