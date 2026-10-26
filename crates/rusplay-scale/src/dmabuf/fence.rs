use std::{os::fd::BorrowedFd, time::Duration};

use rustix::{
    event::{PollFd, PollFlags, Timespec},
    io::Errno,
};

use crate::ScaleError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteFence {
    Signalled,
    TimedOut,
}

pub fn wait_for_writes(fd: BorrowedFd<'_>, timeout: Duration) -> Result<WriteFence, ScaleError> {
    let timeout = Timespec::try_from(timeout).unwrap_or(Timespec {
        tv_sec: i64::MAX,
        tv_nsec: 0,
    });

    loop {
        let mut fds = [PollFd::new(&fd, PollFlags::IN)];
        match rustix::event::poll(&mut fds, Some(&timeout)) {
            Ok(0) => return Ok(WriteFence::TimedOut),
            Ok(_) => return Ok(WriteFence::Signalled),
            Err(Errno::INTR) => {}
            Err(errno) => return Err(ScaleError::DmaBufFencePoll(errno)),
        }
    }
}
