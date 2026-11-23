//! Decides what one run of the binary does, from the name it was started under and its arguments.

use std::ffi::{OsStr, OsString};
use std::iter;
use std::path::Path;

use clap::error::ErrorKind;
use clap::{Parser, Subcommand};

/// Base name of `argv[0]` that makes the binary act as the C++ `librepods-ctl`, with no subcommand.
const LEGACY_CLIENT_NAME: &str = "librepods-ctl";

const CTL_SUBCOMMAND_PROGRAM: &str = "librepods ctl";

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Invocation {
    LegacyClient { verb: String },
    LegacyUsage { program: &'static str },
    Daemon,
    Status,
    Watch,
    Set { key: String, value: String },
    Do { action: String },
    StateDir,
}

/// Daemon for the Omarchy bar plugin, and its command line client.
#[derive(Debug, Parser)]
#[command(name = "librepods", version, subcommand_required = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Send one command to the running daemon, exactly as librepods-ctl does
    #[command(disable_help_flag = true)]
    Ctl {
        /// The command to send; anything after it is ignored, as librepods-ctl ignores it
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        words: Vec<OsString>,
    },
    /// Run the daemon; `--headless` is the name the C++ daemon's service file starts it under
    #[command(long_flag = "headless")]
    Daemon,
    /// Print the status line
    Status,
    /// Print the status line now and again on every change, until the daemon stops
    Watch,
    /// Change one setting, such as noise or the adaptive level
    Set {
        /// The setting to change
        key: String,
        /// Its new value, such as anc or 40
        value: String,
    },
    /// Ask for one action: connect, disconnect or forget
    Do {
        /// The action to ask for
        action: String,
    },
    /// Print the directory that holds the state file, the settings and the pairing keys
    StateDir,
}

// Sample input: `/bin/librepods-ctl` with `["noise:anc"]`, or `librepods` with `["ctl", "status"]`
pub(crate) fn parse(argv0: &OsStr, args: &[OsString]) -> Result<Invocation, clap::Error> {
    if Path::new(argv0).file_name() == Some(OsStr::new(LEGACY_CLIENT_NAME)) {
        return legacy(LEGACY_CLIENT_NAME, args);
    }
    let cli = Cli::try_parse_from(iter::once(argv0).chain(args.iter().map(OsString::as_os_str)))?;
    match cli.command {
        Command::Ctl { words } => legacy(CTL_SUBCOMMAND_PROGRAM, &words),
        Command::Daemon => Ok(Invocation::Daemon),
        Command::Status => Ok(Invocation::Status),
        Command::Watch => Ok(Invocation::Watch),
        Command::Set { key, value } => Ok(Invocation::Set { key, value }),
        Command::Do { action } => Ok(Invocation::Do { action }),
        Command::StateDir => Ok(Invocation::StateDir),
    }
}

fn legacy(program: &'static str, words: &[OsString]) -> Result<Invocation, clap::Error> {
    let Some(verb) = words.first() else {
        return Ok(Invocation::LegacyUsage { program });
    };
    match verb.to_str() {
        Some(verb) => Ok(Invocation::LegacyClient {
            verb: verb.to_owned(),
        }),
        None => Err(clap::Error::new(ErrorKind::InvalidUtf8)),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::OsStringExt;
    use std::path::Path;

    use clap::error::ErrorKind;

    use super::{Invocation, parse};

    fn args(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    fn client(verb: &str) -> Invocation {
        Invocation::LegacyClient {
            verb: verb.to_owned(),
        }
    }

    #[test]
    fn legacy_name_takes_the_verb_with_no_subcommand() {
        let parsed = parse(OsStr::new("/usr/bin/librepods-ctl"), &args(&["noise:anc"]));
        assert_eq!(parsed.unwrap(), client("noise:anc"));
    }

    #[test]
    fn legacy_name_sends_flags_as_verbs_and_ignores_extra_words() {
        let parsed = parse(OsStr::new("librepods-ctl"), &args(&["--help", "extra"]));
        assert_eq!(parsed.unwrap(), client("--help"));
    }

    #[test]
    fn legacy_name_without_a_verb_is_usage() {
        let parsed = parse(OsStr::new("librepods-ctl"), &[]);
        assert_eq!(
            parsed.unwrap(),
            Invocation::LegacyUsage {
                program: "librepods-ctl"
            }
        );
    }

    #[test]
    fn ctl_subcommand_reaches_the_same_client() {
        let parsed = parse(OsStr::new("librepods"), &args(&["ctl", "status", "extra"]));
        assert_eq!(parsed.unwrap(), client("status"));
    }

    #[test]
    fn ctl_subcommand_sends_flags_as_verbs() {
        let parsed = parse(OsStr::new("librepods"), &args(&["ctl", "--help", "extra"]));
        assert_eq!(parsed.unwrap(), client("--help"));
    }

    #[test]
    fn ctl_subcommand_without_a_verb_is_usage() {
        let parsed = parse(OsStr::new("librepods"), &args(&["ctl"]));
        assert_eq!(
            parsed.unwrap(),
            Invocation::LegacyUsage {
                program: "librepods ctl"
            }
        );
    }

    #[test]
    fn a_name_that_only_contains_librepods_ctl_is_not_the_client() {
        let parsed = parse(OsStr::new("librepods-ctl.old"), &args(&["noise:anc"]));
        assert_eq!(parsed.unwrap_err().kind(), ErrorKind::InvalidSubcommand);
    }

    #[test]
    fn no_arguments_is_a_usage_error() {
        let parsed = parse(OsStr::new("librepods"), &[]);
        assert_eq!(
            parsed.unwrap_err().kind(),
            ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
    }

    #[test]
    fn daemon_subcommand_and_headless_flag_both_start_the_daemon() {
        for words in [["daemon"], ["--headless"]] {
            let parsed = parse(OsStr::new("librepods"), &args(&words));
            assert_eq!(parsed.unwrap(), Invocation::Daemon, "{words:?}");
        }
    }

    #[test]
    fn the_packaged_unit_starts_the_daemon() {
        let unit = include_str!("../../../packaging/systemd/librepods.service");
        // Sample input: `ExecStart=%h/.local/bin/librepods daemon`.
        let line = unit
            .lines()
            .find_map(|line| line.strip_prefix("ExecStart="))
            .unwrap();
        let mut words = line.split_whitespace();
        let program = Path::new(words.next().unwrap()).file_name().unwrap();
        let rest: Vec<&str> = words.collect();
        assert_eq!(parse(program, &args(&rest)).unwrap(), Invocation::Daemon);
    }

    #[test]
    fn headless_flag_takes_no_further_arguments() {
        let parsed = parse(OsStr::new("librepods"), &args(&["--headless", "--debug"]));
        assert_eq!(parsed.unwrap_err().kind(), ErrorKind::UnknownArgument);
    }

    #[test]
    fn protocol_1_subcommands_take_their_words() {
        for (words, expected) in [
            (vec!["status"], Invocation::Status),
            (vec!["watch"], Invocation::Watch),
            (
                vec!["set", "noise", "anc"],
                Invocation::Set {
                    key: "noise".to_owned(),
                    value: "anc".to_owned(),
                },
            ),
            (
                vec!["do", "disconnect"],
                Invocation::Do {
                    action: "disconnect".to_owned(),
                },
            ),
            (vec!["state-dir"], Invocation::StateDir),
        ] {
            let parsed = parse(OsStr::new("librepods"), &args(&words));
            assert_eq!(parsed.unwrap(), expected, "{words:?}");
        }
    }

    #[test]
    fn set_needs_a_key_and_a_value() {
        let parsed = parse(OsStr::new("librepods"), &args(&["set", "noise"]));
        assert_eq!(
            parsed.unwrap_err().kind(),
            ErrorKind::MissingRequiredArgument
        );
        let parsed = parse(OsStr::new("librepods"), &args(&["status", "extra"]));
        assert_eq!(parsed.unwrap_err().kind(), ErrorKind::UnknownArgument);
    }

    #[test]
    fn verb_that_is_not_utf8_is_refused() {
        let verb = OsString::from_vec(vec![0xff]);
        let parsed = parse(OsStr::new("librepods-ctl"), &[verb]);
        assert_eq!(parsed.unwrap_err().kind(), ErrorKind::InvalidUtf8);
    }
}
