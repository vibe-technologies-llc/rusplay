use std::{ffi::OsString, path::PathBuf};

use lexopt::{Arg, Parser, ValueExt};
use thiserror::Error;

pub const HELP: &str = "\
Headless Rusplay host

Usage: rusplay-hostd [OPTIONS]
       rusplay-hostd [OPTIONS] peers list
       rusplay-hostd [OPTIONS] peers forget <ID|NAME>

Commands:
  peers list           Print the paired clients, one `<id> <name>` per line
  peers forget <ID|NAME>
                       Revoke a paired client by its id (or an unambiguous prefix of at
                       least 4 characters) or by its exact name. A running daemon notices on
                       the next pairing or reconnect attempt; a client already streaming
                       stays until it leaves.

Options:
  -c, --config <PATH>  Read the configuration from PATH
  -h, --help           Print this help
  -V, --version        Print the version

Without --config the first of these that exists is used, otherwise the built-in defaults:
  $XDG_CONFIG_HOME/rusplay/host.toml
  ~/.config/rusplay/host.toml
";

pub const VERSION: &str = concat!("rusplay-hostd ", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Error)]
pub enum CliError {
    #[error(transparent)]
    Parse(#[from] lexopt::Error),
    #[error("--config was given more than once")]
    ConfigRepeated,
    #[error("unknown command {0:?}")]
    UnknownCommand(String),
    #[error("peers needs an action, list or forget")]
    PeersActionMissing,
    #[error("unknown peers action {0:?}")]
    UnknownPeersAction(String),
    #[error("peers forget needs an id or a name")]
    ForgetTargetMissing,
    #[error("unexpected argument {0:?}")]
    UnexpectedArgument(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeersAction {
    List,
    Forget { target: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Run {
        config: Option<PathBuf>,
    },
    Peers {
        config: Option<PathBuf>,
        action: PeersAction,
    },
    Help,
    Version,
}

pub fn parse<I>(args: I) -> Result<Command, CliError>
where
    I: IntoIterator,
    I::Item: Into<OsString>,
{
    let mut parser = Parser::from_args(args);
    let mut config = None;
    let mut words = Vec::new();

    while let Some(arg) = parser.next()? {
        match arg {
            Arg::Short('h') | Arg::Long("help") => return Ok(Command::Help),
            Arg::Short('V') | Arg::Long("version") => return Ok(Command::Version),
            Arg::Short('c') | Arg::Long("config") => {
                if config.is_some() {
                    return Err(CliError::ConfigRepeated);
                }
                config = Some(PathBuf::from(parser.value()?));
            }
            Arg::Value(word) => words.push(word.string()?),
            other => return Err(other.unexpected().into()),
        }
    }

    let mut words = words.into_iter();
    let Some(command) = words.next() else {
        return Ok(Command::Run { config });
    };
    if command != "peers" {
        return Err(CliError::UnknownCommand(command));
    }

    let action = match words.next().as_deref() {
        None => return Err(CliError::PeersActionMissing),
        Some("list") => PeersAction::List,
        Some("forget") => PeersAction::Forget {
            target: words.next().ok_or(CliError::ForgetTargetMissing)?,
        },
        Some(other) => return Err(CliError::UnknownPeersAction(other.to_owned())),
    };
    if let Some(extra) = words.next() {
        return Err(CliError::UnexpectedArgument(extra));
    }

    Ok(Command::Peers { config, action })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(args: &[&str]) -> Result<Command, CliError> {
        parse(args.iter().copied())
    }

    #[test]
    fn no_arguments_run_with_discovery() {
        let command = parse_strs(&[]).unwrap();

        assert_eq!(command, Command::Run { config: None });
    }

    #[test]
    fn config_takes_a_path_in_every_spelling() {
        for args in [
            &["--config", "a.toml"][..],
            &["--config=a.toml"],
            &["-c", "a.toml"],
            &["-ca.toml"],
        ] {
            let command = parse_strs(args).unwrap();

            assert_eq!(
                command,
                Command::Run {
                    config: Some(PathBuf::from("a.toml"))
                }
            );
        }
    }

    #[test]
    fn help_and_version_short_circuit() {
        assert_eq!(parse_strs(&["--help"]).unwrap(), Command::Help);
        assert_eq!(parse_strs(&["-h"]).unwrap(), Command::Help);
        assert_eq!(parse_strs(&["--version"]).unwrap(), Command::Version);
        assert_eq!(parse_strs(&["-V"]).unwrap(), Command::Version);
        assert_eq!(
            parse_strs(&["--config", "a.toml", "--help"]).unwrap(),
            Command::Help
        );
    }

    #[test]
    fn missing_config_value_is_an_error() {
        let error = parse_strs(&["--config"]).unwrap_err();

        assert!(matches!(
            error,
            CliError::Parse(lexopt::Error::MissingValue { .. })
        ));
    }

    #[test]
    fn unknown_option_is_an_error() {
        let error = parse_strs(&["--frobnicate"]).unwrap_err();

        assert!(matches!(
            error,
            CliError::Parse(lexopt::Error::UnexpectedOption(_))
        ));
    }

    #[test]
    fn unknown_command_is_an_error() {
        let error = parse_strs(&["host.toml"]).unwrap_err();

        assert!(matches!(error, CliError::UnknownCommand(word) if word == "host.toml"));
    }

    #[test]
    fn peers_list_and_forget_parse_with_or_without_a_config() {
        assert_eq!(
            parse_strs(&["peers", "list"]).unwrap(),
            Command::Peers {
                config: None,
                action: PeersAction::List
            }
        );
        assert_eq!(
            parse_strs(&["-c", "a.toml", "peers", "forget", "desk pc"]).unwrap(),
            Command::Peers {
                config: Some(PathBuf::from("a.toml")),
                action: PeersAction::Forget {
                    target: "desk pc".to_owned()
                }
            }
        );
        assert_eq!(
            parse_strs(&["peers", "forget", "ab12", "--config", "a.toml"]).unwrap(),
            Command::Peers {
                config: Some(PathBuf::from("a.toml")),
                action: PeersAction::Forget {
                    target: "ab12".to_owned()
                }
            }
        );
    }

    #[test]
    fn incomplete_or_unknown_peers_commands_are_errors() {
        assert!(matches!(
            parse_strs(&["peers"]),
            Err(CliError::PeersActionMissing)
        ));
        assert!(matches!(
            parse_strs(&["peers", "forget"]),
            Err(CliError::ForgetTargetMissing)
        ));
        assert!(matches!(
            parse_strs(&["peers", "rename"]),
            Err(CliError::UnknownPeersAction(word)) if word == "rename"
        ));
        assert!(matches!(
            parse_strs(&["peers", "list", "extra"]),
            Err(CliError::UnexpectedArgument(word)) if word == "extra"
        ));
    }

    #[test]
    fn repeated_config_is_an_error() {
        let error = parse_strs(&["-c", "a.toml", "-c", "b.toml"]).unwrap_err();

        assert!(matches!(error, CliError::ConfigRepeated));
    }

    #[test]
    fn version_names_the_binary() {
        assert!(VERSION.starts_with("rusplay-hostd "));
    }
}
