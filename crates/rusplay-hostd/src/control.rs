use std::{
    fmt::{self, Display},
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    net::Shutdown,
    os::unix::{fs::FileTypeExt, net::UnixStream},
    path::{Path, PathBuf},
    time::Duration,
};

use rusplay_config::PeersConfig;
use thiserror::Error;

const RUNTIME_DIRECTORY: &str = "rusplay";
const FILE_NAME: &str = "control.sock";
pub const MAX_REQUEST_BYTES: usize = 256;
const MAX_REPLY_BYTES: u64 = 16 * 1024;
const CLIENT_TIMEOUT: Duration = Duration::from_secs(5);
const OK: &str = "ok";
const ERR: &str = "err";

#[derive(Debug, Error)]
pub enum ControlError {
    #[error(
        "no control socket path: peers.control_socket_path is unset and XDG_RUNTIME_DIR is not an absolute path"
    )]
    NoRuntimeDirectory,
    #[error("failed to create the control socket directory {}", path.display())]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{} exists and is not a socket", path.display())]
    NotASocket { path: PathBuf },
    #[error("another daemon already answers on {}", path.display())]
    AlreadyRunning { path: PathBuf },
    #[error("failed to bind the control socket {}", path.display())]
    Bind {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to spawn the control thread")]
    Spawn(#[source] io::Error),
    #[error("no daemon answers on {}", path.display())]
    NotRunning { path: PathBuf },
    #[error("failed to talk to the control socket {}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the daemon's reply on {} is not a valid reply", path.display())]
    BadReply { path: PathBuf },
    #[error("the daemon refused the request: {0}")]
    Refused(Refusal),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Empty,
    TooLong,
    Malformed,
    UnknownRequest(String),
    UnexpectedArgument(String),
    ForgetTargetMissing,
    Streaming,
    PinUnavailable,
    NotFound { target: String },
    Ambiguous { target: String, count: usize },
    StoreFailed,
}

impl Refusal {
    pub fn to_wire(&self) -> String {
        match self {
            Self::Empty => "empty".to_owned(),
            Self::TooLong => "too-long".to_owned(),
            Self::Malformed => "malformed".to_owned(),
            Self::UnknownRequest(word) => format!("unknown-request {word}"),
            Self::UnexpectedArgument(word) => format!("unexpected-argument {word}"),
            Self::ForgetTargetMissing => "forget-target-missing".to_owned(),
            Self::Streaming => "streaming".to_owned(),
            Self::PinUnavailable => "pin-unavailable".to_owned(),
            Self::NotFound { target } => format!("not-found {target}"),
            Self::Ambiguous { target, count } => format!("ambiguous {count} {target}"),
            Self::StoreFailed => "store-failed".to_owned(),
        }
    }

    pub fn from_wire(text: &str) -> Option<Self> {
        let (code, detail) = text.split_once(' ').unwrap_or((text, ""));

        Some(match code {
            "empty" => Self::Empty,
            "too-long" => Self::TooLong,
            "malformed" => Self::Malformed,
            "unknown-request" => Self::UnknownRequest(detail.to_owned()),
            "unexpected-argument" => Self::UnexpectedArgument(detail.to_owned()),
            "forget-target-missing" => Self::ForgetTargetMissing,
            "streaming" => Self::Streaming,
            "pin-unavailable" => Self::PinUnavailable,
            "not-found" => Self::NotFound {
                target: detail.to_owned(),
            },
            "ambiguous" => {
                let (count, target) = detail.split_once(' ')?;
                Self::Ambiguous {
                    target: target.to_owned(),
                    count: count.parse().ok()?,
                }
            }
            "store-failed" => Self::StoreFailed,
            _ => return None,
        })
    }
}

impl Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(formatter, "the request is empty"),
            Self::TooLong => write!(formatter, "the request is above {MAX_REQUEST_BYTES} bytes"),
            Self::Malformed => write!(formatter, "the request is not one line of plain text"),
            Self::UnknownRequest(word) => write!(formatter, "unknown request {word:?}"),
            Self::UnexpectedArgument(word) => write!(formatter, "unexpected argument {word:?}"),
            Self::ForgetTargetMissing => write!(formatter, "forget needs an id or a name"),
            Self::Streaming => write!(
                formatter,
                "a client is streaming, no pairing window is open"
            ),
            Self::PinUnavailable => write!(formatter, "a pin could not be generated"),
            Self::NotFound { target } => write!(formatter, "no paired client matches {target:?}"),
            Self::Ambiguous { target, count } => write!(
                formatter,
                "{target:?} matches {count} paired clients, use more of the id"
            ),
            Self::StoreFailed => write!(formatter, "the peer store could not be updated"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Status,
    NewPin,
    Forget { target: String },
}

impl Request {
    pub fn parse(line: &[u8]) -> Result<Self, Refusal> {
        let text = std::str::from_utf8(line).map_err(|_| Refusal::Malformed)?;
        let text = text.trim_end_matches(['\r', '\n']);
        if text.chars().any(char::is_control) {
            return Err(Refusal::Malformed);
        }

        let text = text.trim();
        let (command, rest) = text.split_once(' ').unwrap_or((text, ""));
        let rest = rest.trim();

        match command {
            "" => Err(Refusal::Empty),
            "status" | "new-pin" if !rest.is_empty() => {
                Err(Refusal::UnexpectedArgument(rest.to_owned()))
            }
            "status" => Ok(Self::Status),
            "new-pin" => Ok(Self::NewPin),
            "forget" if rest.is_empty() => Err(Refusal::ForgetTargetMissing),
            "forget" => Ok(Self::Forget {
                target: rest.to_owned(),
            }),
            other => Err(Refusal::UnknownRequest(other.to_owned())),
        }
    }

    pub fn render(&self) -> String {
        match self {
            Self::Status => "status\n".to_owned(),
            Self::NewPin => "new-pin\n".to_owned(),
            Self::Forget { target } => format!("forget {target}\n"),
        }
    }
}

pub fn render_reply(reply: &Result<String, Refusal>) -> String {
    match reply {
        Ok(body) => format!("{OK}\n{body}"),
        Err(refusal) => format!("{ERR} {}\n", refusal.to_wire()),
    }
}

pub fn parse_reply(text: &str) -> Option<Result<String, Refusal>> {
    let (first, body) = text.split_once('\n')?;
    if first == OK {
        return Some(Ok(body.to_owned()));
    }

    let refusal = first.strip_prefix(ERR)?.strip_prefix(' ')?;

    Refusal::from_wire(refusal).map(Err)
}

pub fn read_request(reader: impl Read) -> io::Result<Result<Request, Refusal>> {
    let limit = u64::try_from(MAX_REQUEST_BYTES + 1).unwrap_or(u64::MAX);
    let mut line = Vec::new();
    BufReader::new(reader.take(limit)).read_until(b'\n', &mut line)?;

    if line.len() > MAX_REQUEST_BYTES {
        return Ok(Err(Refusal::TooLong));
    }

    Ok(Request::parse(&line))
}

pub fn resolve(peers: &PeersConfig, runtime_dir: Option<&Path>) -> Result<PathBuf, ControlError> {
    if let Some(path) = &peers.control_socket_path {
        return Ok(path.clone());
    }

    runtime_dir
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(RUNTIME_DIRECTORY).join(FILE_NAME))
        .ok_or(ControlError::NoRuntimeDirectory)
}

pub fn send(path: &Path, request: &Request) -> Result<String, ControlError> {
    let io_error = |source| ControlError::Io {
        path: path.to_owned(),
        source,
    };

    let mut stream = UnixStream::connect(path).map_err(|source| match source.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => ControlError::NotRunning {
            path: path.to_owned(),
        },
        _ => io_error(source),
    })?;
    stream
        .set_read_timeout(Some(CLIENT_TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(CLIENT_TIMEOUT)))
        .and_then(|()| stream.write_all(request.render().as_bytes()))
        .and_then(|()| stream.shutdown(Shutdown::Write))
        .map_err(io_error)?;

    let mut reply = Vec::new();
    (&stream)
        .take(MAX_REPLY_BYTES)
        .read_to_end(&mut reply)
        .map_err(io_error)?;

    let text = String::from_utf8(reply).map_err(|_| ControlError::BadReply {
        path: path.to_owned(),
    })?;

    match parse_reply(&text) {
        Some(Ok(body)) => Ok(body),
        Some(Err(refusal)) => Err(ControlError::Refused(refusal)),
        None => Err(ControlError::BadReply {
            path: path.to_owned(),
        }),
    }
}

pub fn remove_stale(path: &Path) -> Result<(), ControlError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(ControlError::Io {
                path: path.to_owned(),
                source,
            });
        }
    };
    if !metadata.file_type().is_socket() {
        return Err(ControlError::NotASocket {
            path: path.to_owned(),
        });
    }

    match UnixStream::connect(path) {
        Ok(_) => Err(ControlError::AlreadyRunning {
            path: path.to_owned(),
        }),
        Err(source) if source.kind() == io::ErrorKind::ConnectionRefused => fs::remove_file(path)
            .map_err(|source| ControlError::Io {
                path: path.to_owned(),
                source,
            }),
        Err(source) => Err(ControlError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn the_default_path_is_under_the_runtime_directory() {
        let peers = PeersConfig::default();

        let path = resolve(&peers, Some(Path::new("/run/user/1000"))).unwrap();

        assert_eq!(path, PathBuf::from("/run/user/1000/rusplay/control.sock"));
    }

    #[test]
    fn a_configured_path_wins_and_a_relative_runtime_directory_is_ignored() {
        let configured = PeersConfig {
            control_socket_path: Some(PathBuf::from("/srv/control")),
            ..PeersConfig::default()
        };

        assert_eq!(
            resolve(&configured, None).unwrap(),
            PathBuf::from("/srv/control")
        );
        assert!(matches!(
            resolve(&PeersConfig::default(), Some(Path::new("relative"))),
            Err(ControlError::NoRuntimeDirectory)
        ));
        assert!(matches!(
            resolve(&PeersConfig::default(), None),
            Err(ControlError::NoRuntimeDirectory)
        ));
    }

    #[test]
    fn requests_parse_and_render_back() {
        for request in [
            Request::Status,
            Request::NewPin,
            Request::Forget {
                target: "desk pc".to_owned(),
            },
        ] {
            assert_eq!(Request::parse(request.render().as_bytes()), Ok(request));
        }
    }

    #[test]
    fn malformed_requests_are_refused_by_kind() {
        assert_eq!(Request::parse(b"\n"), Err(Refusal::Empty));
        assert_eq!(Request::parse(b"\xff\n"), Err(Refusal::Malformed));
        assert_eq!(Request::parse(b"sta\x07tus\n"), Err(Refusal::Malformed));
        assert_eq!(
            Request::parse(b"reboot\n"),
            Err(Refusal::UnknownRequest("reboot".to_owned()))
        );
        assert_eq!(
            Request::parse(b"status now\n"),
            Err(Refusal::UnexpectedArgument("now".to_owned()))
        );
        assert_eq!(
            Request::parse(b"forget\n"),
            Err(Refusal::ForgetTargetMissing)
        );
        assert_eq!(
            Request::parse(b"forget   \n"),
            Err(Refusal::ForgetTargetMissing)
        );
    }

    #[test]
    fn a_request_is_bounded() {
        let long = vec![b'a'; MAX_REQUEST_BYTES * 8];
        let exact = format!(
            "forget {}\n",
            "a".repeat(MAX_REQUEST_BYTES - "forget \n".len())
        );

        let refused = read_request(long.as_slice()).unwrap();
        let accepted = read_request(exact.as_bytes()).unwrap();

        assert_eq!(refused, Err(Refusal::TooLong));
        assert!(matches!(accepted, Ok(Request::Forget { .. })));
    }

    #[test]
    fn a_request_without_a_newline_still_parses_at_end_of_input() {
        let request = read_request(&b"status"[..]).unwrap();

        assert_eq!(request, Ok(Request::Status));
    }

    #[test]
    fn replies_round_trip_through_the_wire() {
        let replies = [
            Ok("listening 0.0.0.0:1\nstate pairing\n".to_owned()),
            Ok(String::new()),
            Err(Refusal::Streaming),
            Err(Refusal::NotFound {
                target: "desk pc".to_owned(),
            }),
            Err(Refusal::Ambiguous {
                target: "same".to_owned(),
                count: 2,
            }),
            Err(Refusal::UnknownRequest("x".to_owned())),
        ];

        for reply in replies {
            assert_eq!(parse_reply(&render_reply(&reply)), Some(reply));
        }
        assert_eq!(parse_reply("garbage\n"), None);
        assert_eq!(parse_reply("err nonsense\n"), None);
        assert_eq!(parse_reply("ok"), None);
    }

    #[test]
    fn sending_to_a_missing_socket_says_no_daemon_runs() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("control.sock");

        let result = send(&path, &Request::Status);

        assert!(matches!(result, Err(ControlError::NotRunning { .. })));
    }

    #[test]
    fn a_stale_socket_is_removed_and_a_live_one_or_a_file_is_refused() {
        let directory = TempDir::new().unwrap();
        let stale = directory.path().join("stale.sock");
        let live = directory.path().join("live.sock");
        let plain = directory.path().join("plain");
        drop(UnixListener::bind(&stale).unwrap());
        let _listener = UnixListener::bind(&live).unwrap();
        fs::write(&plain, "x").unwrap();

        assert!(stale.exists());
        remove_stale(&stale).unwrap();

        assert!(!stale.exists());
        assert!(matches!(
            remove_stale(&live),
            Err(ControlError::AlreadyRunning { .. })
        ));
        assert!(matches!(
            remove_stale(&plain),
            Err(ControlError::NotASocket { .. })
        ));
        assert!(remove_stale(&directory.path().join("absent")).is_ok());
    }
}
