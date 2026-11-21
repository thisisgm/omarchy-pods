//! Decides what one run of the binary does, from the name it was started under and its arguments.

use std::ffi::{OsStr, OsString};
use std::iter;
use std::path::Path;

use clap::error::ErrorKind;
use clap::{Parser, Subcommand};

/// Base name of `argv[0]` that makes the binary act as the C++ `librepods-ctl`, with no subcommand.
const LEGACY_CLIENT_NAME: &str = "librepods-ctl";

/// Program name the client usage shows when it is reached as `librepods ctl`.
const CTL_SUBCOMMAND_PROGRAM: &str = "librepods ctl";

/// What one run of the binary does.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Invocation {
    /// Send one verb to the daemon, as `librepods-ctl <verb>` does.
    LegacyClient { verb: String },
    /// Print the client usage under this program name and fail, as `librepods-ctl` with no verb does.
    LegacyUsage { program: &'static str },
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
}

/// Parses `argv[0]` and the arguments after it.
pub(crate) fn parse(argv0: &OsStr, args: &[OsString]) -> Result<Invocation, clap::Error> {
    if Path::new(argv0).file_name() == Some(OsStr::new(LEGACY_CLIENT_NAME)) {
        return legacy(LEGACY_CLIENT_NAME, args);
    }
    let cli = Cli::try_parse_from(iter::once(argv0).chain(args.iter().map(OsString::as_os_str)))?;
    match cli.command {
        Command::Ctl { words } => legacy(CTL_SUBCOMMAND_PROGRAM, &words),
    }
}

/// The client invocation for these words: the first is the verb, the rest are ignored.
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
        let parsed = parse(OsStr::new("librepods-ctl.old"), &args(&["status"]));
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
    fn verb_that_is_not_utf8_is_refused() {
        let verb = OsString::from_vec(vec![0xff]);
        let parsed = parse(OsStr::new("librepods-ctl"), &[verb]);
        assert_eq!(parsed.unwrap_err().kind(), ErrorKind::InvalidUtf8);
    }
}
