use std::{ffi::OsString, path::PathBuf};

use lexopt::{Arg, Parser};
use thiserror::Error;

pub const HELP: &str = "\
Headless Rusplay host

Usage: rusplay-hostd [OPTIONS]

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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Run { config: Option<PathBuf> },
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
            other => return Err(other.unexpected().into()),
        }
    }

    Ok(Command::Run { config })
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
    fn positional_argument_is_an_error() {
        let error = parse_strs(&["host.toml"]).unwrap_err();

        assert!(matches!(
            error,
            CliError::Parse(lexopt::Error::UnexpectedArgument(_))
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
