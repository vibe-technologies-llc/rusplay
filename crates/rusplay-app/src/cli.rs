use std::ffi::OsString;

use lexopt::{Arg, Parser, ValueExt};
use thiserror::Error;

pub const HELP: &str = "\
Rusplay desktop app

Usage: rusplay [OPTIONS]

Options:
      --connect <NAME|ADDRESS>
                       Connect to a host of the host book as soon as the app opens, with
                       its profile applied. The host must already be paired.
      --exit-on-disconnect
                       Quit when the session ends or fails to start
  -h, --help           Print this help
  -V, --version        Print the version
";

pub const VERSION: &str = concat!("rusplay ", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Error)]
pub enum CliError {
    #[error(transparent)]
    Parse(#[from] lexopt::Error),
    #[error("--connect was given more than once")]
    ConnectRepeated,
    #[error("--connect needs a host name or address")]
    ConnectTargetEmpty,
    #[error("unexpected argument {0:?}")]
    UnexpectedArgument(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Launch {
    pub connect: Option<String>,
    pub exit_on_disconnect: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Run(Launch),
    Help,
    Version,
}

pub fn parse<I>(args: I) -> Result<Command, CliError>
where
    I: IntoIterator,
    I::Item: Into<OsString>,
{
    let mut parser = Parser::from_args(args);
    let mut launch = Launch::default();

    while let Some(arg) = parser.next()? {
        match arg {
            Arg::Short('h') | Arg::Long("help") => return Ok(Command::Help),
            Arg::Short('V') | Arg::Long("version") => return Ok(Command::Version),
            Arg::Long("connect") => {
                if launch.connect.is_some() {
                    return Err(CliError::ConnectRepeated);
                }
                let target = parser.value()?.string()?;
                if target.trim().is_empty() {
                    return Err(CliError::ConnectTargetEmpty);
                }
                launch.connect = Some(target);
            }
            Arg::Long("exit-on-disconnect") => launch.exit_on_disconnect = true,
            Arg::Value(word) => return Err(CliError::UnexpectedArgument(word.string()?)),
            other => return Err(other.unexpected().into()),
        }
    }

    Ok(Command::Run(launch))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(args: &[&str]) -> Result<Command, CliError> {
        parse(args.iter().copied())
    }

    #[test]
    fn no_arguments_open_the_app_normally() {
        assert_eq!(parse_strs(&[]).unwrap(), Command::Run(Launch::default()));
    }

    #[test]
    fn connect_takes_a_target_in_every_spelling() {
        for args in [&["--connect", "desk"][..], &["--connect=desk"]] {
            assert_eq!(
                parse_strs(args).unwrap(),
                Command::Run(Launch {
                    connect: Some("desk".to_owned()),
                    exit_on_disconnect: false,
                })
            );
        }
    }

    #[test]
    fn exit_on_disconnect_combines_with_connect() {
        let command = parse_strs(&["--exit-on-disconnect", "--connect", "10.0.0.2:47800"]);

        assert_eq!(
            command.unwrap(),
            Command::Run(Launch {
                connect: Some("10.0.0.2:47800".to_owned()),
                exit_on_disconnect: true,
            })
        );
    }

    #[test]
    fn help_and_version_short_circuit() {
        assert_eq!(parse_strs(&["--help"]).unwrap(), Command::Help);
        assert_eq!(parse_strs(&["-h"]).unwrap(), Command::Help);
        assert_eq!(parse_strs(&["--version"]).unwrap(), Command::Version);
        assert_eq!(parse_strs(&["-V"]).unwrap(), Command::Version);
    }

    #[test]
    fn a_missing_or_empty_target_is_an_error() {
        assert!(matches!(
            parse_strs(&["--connect"]).unwrap_err(),
            CliError::Parse(lexopt::Error::MissingValue { .. })
        ));
        assert!(matches!(
            parse_strs(&["--connect", " "]).unwrap_err(),
            CliError::ConnectTargetEmpty
        ));
    }

    #[test]
    fn repeated_connect_and_stray_words_are_errors() {
        assert!(matches!(
            parse_strs(&["--connect", "a", "--connect", "b"]).unwrap_err(),
            CliError::ConnectRepeated
        ));
        assert!(matches!(
            parse_strs(&["desk"]).unwrap_err(),
            CliError::UnexpectedArgument(word) if word == "desk"
        ));
        assert!(matches!(
            parse_strs(&["--frobnicate"]).unwrap_err(),
            CliError::Parse(lexopt::Error::UnexpectedOption(_))
        ));
    }
}
