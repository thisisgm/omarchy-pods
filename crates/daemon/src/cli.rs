//! Decides what one run of the binary does, from the name it was started under and its arguments.

#![forbid(unsafe_code)]

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::path::Path;

/// Base name of `argv[0]` that makes the binary act as the C++ `librepods-ctl`, with no subcommand.
const LEGACY_CLIENT_NAME: &str = "librepods-ctl";

const CTL_SUBCOMMAND_PROGRAM: &str = "librepods ctl";
const VERSION: &str = concat!("librepods ", env!("CARGO_PKG_VERSION"), "\n");
const INVALID_UTF8: &str = "invalid UTF-8 was detected in one or more arguments";
const HELP_OPTION: &str = "  -h, --help  Print help\n";
const ROOT_HELP: Help<'static> = Help {
    usage: "<COMMAND>",
    summary: "Daemon for the Omarchy bar plugin, and its command line client",
    arguments: &[],
    options: "  -h, --help     Print help\n  -V, --version  Print version\n",
};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParseErrorKind {
    DisplayHelp,
    DisplayVersion,
    DisplayHelpOnMissingArgumentOrSubcommand,
    InvalidSubcommand,
    MissingRequiredArgument,
    UnknownArgument,
    InvalidUtf8,
}

#[derive(Debug)]
pub(crate) struct ParseError {
    kind: ParseErrorKind,
    message: String,
}

impl ParseError {
    fn new(kind: ParseErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    fn usage(kind: ParseErrorKind, reason: &str, help: &Help<'_>) -> Self {
        let footer = if help.options.is_empty() {
            ""
        } else {
            "\nFor more information, try '--help'.\n"
        };
        Self::new(
            kind,
            format!(
                "error: {reason}\n\nUsage: librepods {}\n{footer}",
                help.usage
            ),
        )
    }

    pub(crate) fn kind(&self) -> ParseErrorKind {
        self.kind
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Command {
    Ctl,
    Daemon,
    Status,
    Watch,
    Set,
    Do,
    StateDir,
    Help,
}

const COMMANDS: [(&str, Command); 8] = [
    ("ctl", Command::Ctl),
    ("daemon", Command::Daemon),
    ("status", Command::Status),
    ("watch", Command::Watch),
    ("set", Command::Set),
    ("do", Command::Do),
    ("state-dir", Command::StateDir),
    ("help", Command::Help),
];

struct Help<'a> {
    usage: &'a str,
    summary: &'static str,
    arguments: &'static [(&'static str, &'static str)],
    options: &'static str,
}

impl Command {
    fn named(name: &str) -> Option<Self> {
        COMMANDS
            .iter()
            .find_map(|&(word, command)| (word == name).then_some(command))
    }

    fn help(self) -> Help<'static> {
        match self {
            Self::Ctl => Help {
                usage: "ctl [WORDS]...",
                summary: "Send one command to the running daemon, exactly as librepods-ctl does",
                arguments: &[(
                    "[WORDS]...",
                    "The command to send; anything after it is ignored, as librepods-ctl ignores it",
                )],
                options: "",
            },
            Self::Daemon => Help {
                usage: "{daemon|--headless}",
                summary: "Run the daemon; `--headless` is the name the C++ daemon's service file starts it under",
                arguments: &[],
                options: HELP_OPTION,
            },
            Self::Status => Help {
                usage: "status",
                summary: "Print the status line",
                arguments: &[],
                options: HELP_OPTION,
            },
            Self::Watch => Help {
                usage: "watch",
                summary: "Print the status line now and again on every change, until the daemon stops",
                arguments: &[],
                options: HELP_OPTION,
            },
            Self::Set => Help {
                usage: "set <KEY> <VALUE>",
                summary: "Change one setting, such as noise or the adaptive level",
                arguments: &[
                    ("<KEY>", "The setting to change"),
                    ("<VALUE>", "Its new value, such as anc or 40"),
                ],
                options: HELP_OPTION,
            },
            Self::Do => Help {
                usage: "do <ACTION>",
                summary: "Ask for one action: connect, disconnect or forget",
                arguments: &[("<ACTION>", "The action to ask for")],
                options: HELP_OPTION,
            },
            Self::StateDir => Help {
                usage: "state-dir",
                summary: "Print the directory that holds the state file, the settings and the pairing keys",
                arguments: &[],
                options: HELP_OPTION,
            },
            Self::Help => Help {
                usage: "help [COMMAND]...",
                summary: "Print this message or the help of the given subcommand(s)",
                arguments: &[("[COMMAND]...", "Print help for the subcommand(s)")],
                options: "",
            },
        }
    }
}

fn display_help(kind: ParseErrorKind, command: Option<Command>) -> ParseError {
    let help = command.map_or(ROOT_HELP, Command::help);
    let mut message = format!("{}\n\nUsage: librepods {}\n", help.summary, help.usage);
    if command.is_none() {
        const DAEMON_LABEL: &str = "daemon, --headless";
        message.push_str("\nCommands:\n");
        for (name, command) in COMMANDS {
            let label = if command == Command::Daemon {
                DAEMON_LABEL
            } else {
                name
            };
            let _written = writeln!(
                message,
                "  {label:width$}  {}",
                command.help().summary,
                width = DAEMON_LABEL.len()
            );
        }
    }
    if let Some(width) = help.arguments.iter().map(|(name, _)| name.len()).max() {
        message.push_str("\nArguments:\n");
        for (name, description) in help.arguments {
            let _written = writeln!(message, "  {name:width$}  {description}");
        }
    }
    if !help.options.is_empty() {
        message.push_str("\nOptions:\n");
        message.push_str(help.options);
    }
    ParseError::new(kind, message)
}

// Sample input: `/bin/librepods-ctl` with `["noise:anc"]`, or `librepods` with `["ctl", "status"]`.
pub(crate) fn parse(argv0: &OsStr, args: &[OsString]) -> Result<Invocation, ParseError> {
    let legacy_name = Path::new(argv0).file_name() == Some(OsStr::new(LEGACY_CLIENT_NAME));
    let invalid_utf8 = || {
        let command = args
            .first()
            .and_then(|word| word.to_str())
            .and_then(|name| {
                if option_name(name) == Some("--headless") {
                    Some(Command::Daemon)
                } else {
                    Command::named(name)
                }
            });
        if legacy_name || command == Some(Command::Ctl) {
            ParseError::new(
                ParseErrorKind::InvalidUtf8,
                format!("error: {INVALID_UTF8}\n"),
            )
        } else {
            ParseError::usage(
                ParseErrorKind::InvalidUtf8,
                INVALID_UTF8,
                &command.map_or(ROOT_HELP, Command::help),
            )
        }
    };
    let args: Vec<_> = args
        .iter()
        .map(|word| word.to_str())
        .collect::<Option<_>>()
        .ok_or_else(invalid_utf8)?;
    if argv0.to_str().is_none() {
        return Err(invalid_utf8());
    }
    if legacy_name {
        return Ok(legacy(LEGACY_CLIENT_NAME, &args));
    }
    match args.as_slice() {
        [] | ["--"] => Err(display_help(
            ParseErrorKind::DisplayHelpOnMissingArgumentOrSubcommand,
            None,
        )),
        ["--", name, ..] if Command::named(name).is_some() => Err(ParseError::usage(
            ParseErrorKind::UnknownArgument,
            &format!(
                "unexpected argument '{name}' found\n\n  tip: subcommand '{name}' exists; to use it, remove the '--' before it"
            ),
            &ROOT_HELP,
        )),
        ["--", name, ..] => Err(invalid_subcommand(name, &ROOT_HELP)),
        [word, rest @ ..] => match option_name(word) {
            Some("--headless") => parse_command(Command::Daemon, rest),
            Some(option) => Err(parse_option(word, option, None)),
            None => match Command::named(word) {
                Some(command) => parse_command(command, rest),
                None => Err(invalid_subcommand(word, &ROOT_HELP)),
            },
        },
    }
}

fn legacy(program: &'static str, words: &[&str]) -> Invocation {
    words
        .first()
        .map_or(Invocation::LegacyUsage { program }, |verb| {
            Invocation::LegacyClient {
                verb: (*verb).to_owned(),
            }
        })
}

fn invalid_subcommand(name: &str, help: &Help<'_>) -> ParseError {
    ParseError::usage(
        ParseErrorKind::InvalidSubcommand,
        &format!("unrecognized subcommand '{name}'"),
        help,
    )
}

// Sample input: `["noise", "--", "-1"]` for `set`.
fn parse_command(command: Command, words: &[&str]) -> Result<Invocation, ParseError> {
    match command {
        Command::Ctl => {
            let operands = words.strip_prefix(&["--"]).unwrap_or(words);
            return Ok(legacy(CTL_SUBCOMMAND_PROGRAM, operands));
        }
        Command::Help => {
            return Err(match words {
                [] => display_help(ParseErrorKind::DisplayHelp, None),
                [name, rest @ ..] => match (Command::named(name), rest) {
                    (Some(topic), []) => display_help(ParseErrorKind::DisplayHelp, Some(topic)),
                    (Some(topic), [extra, ..]) => invalid_subcommand(extra, &topic.help()),
                    (None, _) => invalid_subcommand(name, &ROOT_HELP),
                },
            });
        }
        Command::Daemon
        | Command::Status
        | Command::Watch
        | Command::Set
        | Command::Do
        | Command::StateDir => {}
    }

    let help = command.help();
    let mut operands = Vec::new();
    let mut literal = false;
    for &word in words {
        if !literal {
            if word == "--" {
                literal = true;
                continue;
            }
            if let Some(option) = option_name(word) {
                return Err(parse_option(word, option, Some(command)));
            }
        }
        if operands.len() == help.arguments.len() {
            return Err(unknown_argument(word, "", &help));
        }
        operands.push(word);
    }

    match (command, operands.as_slice()) {
        (Command::Daemon, []) => Ok(Invocation::Daemon),
        (Command::Status, []) => Ok(Invocation::Status),
        (Command::Watch, []) => Ok(Invocation::Watch),
        (Command::StateDir, []) => Ok(Invocation::StateDir),
        (Command::Set, [key, value]) => Ok(Invocation::Set {
            key: (*key).to_owned(),
            value: (*value).to_owned(),
        }),
        (Command::Do, [action]) => Ok(Invocation::Do {
            action: (*action).to_owned(),
        }),
        _ => {
            let mut message = String::from("the following required arguments were not provided:");
            for (name, _) in help.arguments.iter().skip(operands.len()) {
                let _written = write!(message, "\n  {name}");
            }
            Err(ParseError::usage(
                ParseErrorKind::MissingRequiredArgument,
                &message,
                &help,
            ))
        }
    }
}

// Sample input: `--help=topic` names `--help`, and `-hx` starts with `-h`.
fn option_name(word: &str) -> Option<&str> {
    let short = word.strip_prefix('-')?;
    if short.starts_with('-') {
        return Some(word.split_once('=').map_or(word, |(name, _)| name));
    }
    let first = short.chars().next()?;
    word.get(..=first.len_utf8())
}

fn parse_option(word: &str, name: &str, command: Option<Command>) -> ParseError {
    let help = command.map_or(ROOT_HELP, Command::help);
    let kind = match name {
        "-h" | "--help" => ParseErrorKind::DisplayHelp,
        "-V" | "--version" if command.is_none() => ParseErrorKind::DisplayVersion,
        _ => {
            let tip = if help.arguments.is_empty() {
                String::new()
            } else {
                format!("\n\n  tip: to pass '{name}' as a value, use '-- {name}'")
            };
            return unknown_argument(name, &tip, &help);
        }
    };
    if name.starts_with("--")
        && let Some((_, value)) = word.split_once('=')
    {
        let usage = match (command, help.usage.split_once(' ')) {
            (None, _) => format!("{name} {}", help.usage),
            (Some(_), Some((command, operands))) => format!("{command} {name} {operands}"),
            (Some(_), None) => format!("{} {name}", help.usage),
        };
        return ParseError::usage(
            ParseErrorKind::UnknownArgument,
            &format!("unexpected value '{value}' for '{name}' found; no more were expected"),
            &Help {
                usage: &usage,
                ..help
            },
        );
    }
    if kind == ParseErrorKind::DisplayVersion {
        ParseError::new(kind, VERSION)
    } else {
        display_help(kind, command)
    }
}

fn unknown_argument(word: &str, tip: &str, help: &Help<'_>) -> ParseError {
    ParseError::usage(
        ParseErrorKind::UnknownArgument,
        &format!("unexpected argument '{word}' found{tip}"),
        help,
    )
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::OsStringExt;
    use std::path::Path;

    use super::{Invocation, ParseErrorKind as ErrorKind, parse};

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

    #[test]
    fn non_utf8_anywhere_is_a_usage_error() {
        let invalid = OsString::from_vec(vec![0xff]);
        for words in [
            vec![invalid.clone()],
            vec![OsString::from("--help"), invalid.clone()],
            vec![
                OsString::from("set"),
                OsString::from("noise"),
                invalid.clone(),
            ],
            vec![
                OsString::from("ctl"),
                OsString::from("status"),
                invalid.clone(),
            ],
        ] {
            let error = parse(OsStr::new("librepods"), &words).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::InvalidUtf8);
        }
        let error = parse(&invalid, &[]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidUtf8);
    }

    #[test]
    fn a_separator_passes_hyphenated_operands_literally() {
        let program = OsStr::new("librepods");
        let words = args(&["set", "adaptive", "--", "-1"]);
        assert_eq!(
            parse(program, &words).unwrap(),
            Invocation::Set {
                key: "adaptive".to_owned(),
                value: "-1".to_owned()
            }
        );
        assert_eq!(
            parse(program, &args(&["ctl", "--", "--help"])).unwrap(),
            client("--help")
        );
        assert_eq!(
            parse(OsStr::new("librepods-ctl"), &args(&["--", "--help"])).unwrap(),
            client("--")
        );
    }
}
