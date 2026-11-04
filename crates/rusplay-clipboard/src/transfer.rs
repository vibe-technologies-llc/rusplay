use std::{
    io,
    os::fd::OwnedFd,
    time::{Duration, Instant},
};

use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    fs::{OFlags, fcntl_getfl, fcntl_setfl},
    io::{Errno, read, write},
    pipe::{PipeFlags, pipe_with},
};

pub(crate) const TEXT_MIME: &str = "text/plain;charset=utf-8";
pub(crate) const PASSWORD_HINT_MIME: &str = "x-kde-passwordManagerHint";
pub(crate) const TRANSFER_TIMEOUT: Duration = Duration::from_secs(2);
const READ_CHUNK: usize = 8192;

pub(crate) fn is_shareable(mimes: &[String]) -> bool {
    mimes.iter().any(|mime| mime == TEXT_MIME)
        && !mimes.iter().any(|mime| mime == PASSWORD_HINT_MIME)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadStep {
    Pending,
    Done,
    Abandoned,
}

#[derive(Debug)]
pub(crate) struct ReadJob {
    fd: OwnedFd,
    bytes: Vec<u8>,
    deadline: Instant,
    max_bytes: usize,
}

impl ReadJob {
    pub(crate) fn open(max_bytes: usize, now: Instant) -> io::Result<(Self, OwnedFd)> {
        let (reader, writer) = pipe_with(PipeFlags::CLOEXEC)?;

        Ok((Self::from_fd(reader, max_bytes, now)?, writer))
    }

    pub(crate) fn from_fd(fd: OwnedFd, max_bytes: usize, now: Instant) -> io::Result<Self> {
        let flags = fcntl_getfl(&fd)?;
        fcntl_setfl(&fd, flags | OFlags::NONBLOCK)?;

        Ok(Self {
            fd,
            bytes: Vec::new(),
            deadline: now + TRANSFER_TIMEOUT,
            max_bytes,
        })
    }

    pub(crate) fn fd(&self) -> &OwnedFd {
        &self.fd
    }

    pub(crate) fn advance(&mut self, now: Instant) -> ReadStep {
        let mut buffer = [0; READ_CHUNK];

        loop {
            match read(&self.fd, &mut buffer) {
                Ok(0) => return ReadStep::Done,
                Ok(count) => {
                    if self.bytes.len() + count > self.max_bytes {
                        return ReadStep::Abandoned;
                    }
                    self.bytes.extend_from_slice(&buffer[..count]);
                }
                Err(Errno::INTR) => {}
                Err(Errno::AGAIN) => {
                    return if now >= self.deadline {
                        ReadStep::Abandoned
                    } else {
                        ReadStep::Pending
                    };
                }
                Err(_) => return ReadStep::Abandoned,
            }
        }
    }

    pub(crate) fn into_text(self) -> Option<String> {
        String::from_utf8(self.bytes)
            .ok()
            .filter(|text| !text.is_empty())
    }
}

pub(crate) fn write_all(fd: &OwnedFd, mut bytes: &[u8], timeout: Duration) -> io::Result<()> {
    let flags = fcntl_getfl(fd)?;
    fcntl_setfl(fd, flags | OFlags::NONBLOCK)?;
    let deadline = Instant::now() + timeout;

    while !bytes.is_empty() {
        match write(fd, bytes) {
            Ok(count) => bytes = &bytes[count..],
            Err(Errno::INTR) => {}
            Err(Errno::AGAIN) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(io::ErrorKind::TimedOut.into());
                }
                let limit = Timespec {
                    tv_sec: i64::try_from(remaining.as_secs()).unwrap_or(i64::MAX),
                    tv_nsec: i64::from(remaining.subsec_nanos()),
                };
                let mut fds = [PollFd::new(fd, PollFlags::OUT)];
                match poll(&mut fds, Some(&limit)) {
                    Ok(_) | Err(Errno::INTR) => {}
                    Err(errno) => return Err(errno.into()),
                }
            }
            Err(errno) => return Err(errno.into()),
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(max_bytes: usize, payload: &[u8]) -> (ReadJob, OwnedFd) {
        let (job, writer) = ReadJob::open(max_bytes, Instant::now()).unwrap();
        write_all(&writer, payload, Duration::from_secs(1)).unwrap();

        (job, writer)
    }

    fn finish(mut job: ReadJob) -> (ReadStep, ReadJob) {
        let step = job.advance(Instant::now());

        (step, job)
    }

    #[test]
    fn text_and_the_password_hint_decide_what_is_shared() {
        let plain = vec![
            "text/plain;charset=utf-8".to_owned(),
            "text/html".to_owned(),
        ];
        let secret = vec![
            "text/plain;charset=utf-8".to_owned(),
            "x-kde-passwordManagerHint".to_owned(),
        ];
        let image = vec!["image/png".to_owned()];
        let legacy = vec!["text/plain".to_owned(), "UTF8_STRING".to_owned()];

        assert!(is_shareable(&plain));
        assert!(!is_shareable(&secret));
        assert!(!is_shareable(&image));
        assert!(!is_shareable(&legacy));
    }

    #[test]
    fn a_closed_pipe_yields_its_text() {
        let (job, writer) = feed(100, "héllo".as_bytes());
        drop(writer);

        let (step, job) = finish(job);

        assert_eq!(step, ReadStep::Done);
        assert_eq!(job.into_text().as_deref(), Some("héllo"));
    }

    #[test]
    fn an_open_pipe_stays_pending_until_the_deadline() {
        let (mut job, _writer) = feed(100, b"partial");

        assert_eq!(job.advance(Instant::now()), ReadStep::Pending);
        assert_eq!(
            job.advance(Instant::now() + TRANSFER_TIMEOUT),
            ReadStep::Abandoned
        );
    }

    #[test]
    fn more_than_the_limit_is_abandoned_without_buffering_it_all() {
        let (job, writer) = feed(10, &[b'a'; 11]);
        drop(writer);

        let (step, _) = finish(job);

        assert_eq!(step, ReadStep::Abandoned);
    }

    #[test]
    fn invalid_utf8_and_empty_selections_are_not_text() {
        let (bad, writer) = feed(100, &[0xFF, 0xFE]);
        drop(writer);
        let (empty, writer) = feed(100, b"");
        drop(writer);

        let (_, bad) = finish(bad);
        let (_, empty) = finish(empty);

        assert_eq!(bad.into_text(), None);
        assert_eq!(empty.into_text(), None);
    }

    #[test]
    fn a_reader_that_never_reads_times_out_the_writer() {
        let (job, writer) = ReadJob::open(10, Instant::now()).unwrap();

        let result = write_all(&writer, &vec![b'a'; 1 << 20], Duration::from_millis(100));

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        drop(job);
    }

    #[test]
    fn writing_to_a_closed_pipe_fails_instead_of_blocking() {
        let (job, writer) = ReadJob::open(10, Instant::now()).unwrap();
        drop(job);

        let result = write_all(&writer, b"x", Duration::from_millis(50));

        assert!(result.is_err());
    }
}
