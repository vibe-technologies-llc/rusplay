use rustix::{
    io::Errno,
    process::{Resource, Rlimit, getrlimit, setrlimit},
    thread::gettid,
};
use zbus::{
    blocking::{Connection, proxy::Builder},
    proxy::CacheProperties,
};

use crate::PriorityError;

const DESTINATION: &str = "org.freedesktop.RealtimeKit1";
const PATH: &str = "/org/freedesktop/RealtimeKit1";
const INTERFACE: &str = "org.freedesktop.RealtimeKit1";
const MAX_PRIORITY_PROPERTY: &str = "MaxRealtimePriority";
const TIME_LIMIT_PROPERTY: &str = "RTTimeUSecMax";
const MAKE_REALTIME_METHOD: &str = "MakeThreadRealtime";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RealtimeLimits {
    pub(crate) max_priority: u32,
    pub(crate) time_limit_us: u64,
}

pub(crate) trait RealtimeService {
    fn limits(&self) -> Result<RealtimeLimits, PriorityError>;

    fn make_realtime(&self, thread: u64, priority: u32) -> Result<(), PriorityError>;
}

pub(crate) struct Rtkit {
    proxy: zbus::blocking::Proxy<'static>,
}

impl Rtkit {
    pub(crate) fn connect() -> Result<Self, PriorityError> {
        let connection = Connection::system().map_err(PriorityError::RtkitUnreachable)?;
        let proxy = Builder::new(&connection)
            .destination(DESTINATION)
            .and_then(|builder| builder.path(PATH))
            .and_then(|builder| builder.interface(INTERFACE))
            .and_then(|builder| builder.cache_properties(CacheProperties::No).build())
            .map_err(PriorityError::RtkitUnreachable)?;

        Ok(Self { proxy })
    }
}

impl RealtimeService for Rtkit {
    fn limits(&self) -> Result<RealtimeLimits, PriorityError> {
        let max_priority: i32 = self
            .proxy
            .get_property(MAX_PRIORITY_PROPERTY)
            .map_err(PriorityError::RtkitLimits)?;
        let time_limit_us: i64 = self
            .proxy
            .get_property(TIME_LIMIT_PROPERTY)
            .map_err(PriorityError::RtkitLimits)?;

        Ok(RealtimeLimits {
            max_priority: u32::try_from(max_priority).unwrap_or(0),
            time_limit_us: u64::try_from(time_limit_us).unwrap_or(0),
        })
    }

    fn make_realtime(&self, thread: u64, priority: u32) -> Result<(), PriorityError> {
        self.proxy
            .call::<_, _, ()>(MAKE_REALTIME_METHOD, &(thread, priority))
            .map_err(|source| PriorityError::RtkitRefused { priority, source })
    }
}

pub(crate) fn current_thread_id() -> u64 {
    u64::from(gettid().as_raw_nonzero().get().cast_unsigned())
}

pub(crate) fn clamp_priority(requested: u8, limits: RealtimeLimits) -> Result<u32, PriorityError> {
    match u32::from(requested).min(limits.max_priority) {
        0 => Err(PriorityError::RtkitNoRealtime),
        granted => Ok(granted),
    }
}

pub(crate) fn lowered_time_limit(current: Rlimit, time_limit_us: u64) -> Option<Rlimit> {
    let within = |value: Option<u64>| value.is_some_and(|value| value <= time_limit_us);

    if within(current.current) && within(current.maximum) {
        return None;
    }

    Some(Rlimit {
        current: Some(time_limit_us),
        maximum: Some(time_limit_us),
    })
}

pub(crate) fn lower_time_limit(time_limit_us: u64) -> Result<(), PriorityError> {
    match lowered_time_limit(getrlimit(Resource::Rttime), time_limit_us) {
        Some(target) => setrlimit(Resource::Rttime, target).map_err(|source: Errno| {
            PriorityError::RealtimeTimeLimit {
                microseconds: time_limit_us,
                source,
            }
        }),
        None => Ok(()),
    }
}

pub(crate) fn request_realtime(
    service: &impl RealtimeService,
    requested: u8,
) -> Result<(), PriorityError> {
    let limits = service.limits()?;
    let priority = clamp_priority(requested, limits)?;

    lower_time_limit(limits.time_limit_us)?;

    service.make_realtime(current_thread_id(), priority)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    struct Fake {
        limits: RealtimeLimits,
        granted: RefCell<Vec<(u64, u32)>>,
    }

    impl Fake {
        fn new(max_priority: u32) -> Self {
            Self {
                limits: RealtimeLimits {
                    max_priority,
                    time_limit_us: u64::MAX,
                },
                granted: RefCell::new(Vec::new()),
            }
        }
    }

    impl RealtimeService for Fake {
        fn limits(&self) -> Result<RealtimeLimits, PriorityError> {
            Ok(self.limits)
        }

        fn make_realtime(&self, thread: u64, priority: u32) -> Result<(), PriorityError> {
            self.granted.borrow_mut().push((thread, priority));
            Ok(())
        }
    }

    fn limits(max_priority: u32) -> RealtimeLimits {
        RealtimeLimits {
            max_priority,
            time_limit_us: 200_000,
        }
    }

    #[test]
    fn the_request_is_capped_at_what_rtkit_grants() {
        assert_eq!(clamp_priority(50, limits(20)).unwrap(), 20);
        assert_eq!(clamp_priority(10, limits(20)).unwrap(), 10);
    }

    #[test]
    fn a_service_with_no_realtime_priority_is_an_error() {
        assert!(matches!(
            clamp_priority(10, limits(0)),
            Err(PriorityError::RtkitNoRealtime)
        ));
        assert!(matches!(
            clamp_priority(0, limits(20)),
            Err(PriorityError::RtkitNoRealtime)
        ));
    }

    #[test]
    fn the_time_limit_is_lowered_only_when_it_exceeds_the_services_bound() {
        let unlimited = Rlimit {
            current: None,
            maximum: None,
        };
        let soft_over = Rlimit {
            current: Some(500_000),
            maximum: Some(100_000),
        };
        let within = Rlimit {
            current: Some(100_000),
            maximum: Some(200_000),
        };
        let bound = Some(Rlimit {
            current: Some(200_000),
            maximum: Some(200_000),
        });

        assert_eq!(lowered_time_limit(unlimited, 200_000), bound);
        assert_eq!(lowered_time_limit(soft_over, 200_000), bound);
        assert_eq!(lowered_time_limit(within, 200_000), None);
    }

    #[test]
    fn a_granted_request_names_the_calling_thread_at_the_clamped_priority() {
        let service = Fake::new(15);

        request_realtime(&service, 40).unwrap();

        assert_eq!(*service.granted.borrow(), vec![(current_thread_id(), 15)]);
    }
}
