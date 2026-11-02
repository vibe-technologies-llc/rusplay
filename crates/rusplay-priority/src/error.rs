use rustix::io::Errno;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PriorityError {
    #[error("nice value {nice} was refused")]
    Nice {
        nice: i8,
        #[source]
        source: Errno,
    },
    #[error("timer slack of {nanos} ns was refused")]
    TimerSlack {
        nanos: u64,
        #[source]
        source: Errno,
    },
    #[error("cannot reach rtkit on the system bus")]
    RtkitUnreachable(#[source] zbus::Error),
    #[error("rtkit did not report its limits")]
    RtkitLimits(#[source] zbus::Error),
    #[error("rtkit grants no realtime priority")]
    RtkitNoRealtime,
    #[error("cannot lower the realtime CPU time limit to {microseconds} us")]
    RealtimeTimeLimit {
        microseconds: u64,
        #[source]
        source: Errno,
    },
    #[error("rtkit refused realtime priority {priority}")]
    RtkitRefused {
        priority: u32,
        #[source]
        source: zbus::Error,
    },
}
