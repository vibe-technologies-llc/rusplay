---
paths:
  - "crates/rusplay-priority/**"
---

# Thread priority

- `rusplay-priority` is the one place a Rusplay thread raises its own scheduling. It builds on
  `rusplay-config` only, so `rusplay-host` and `rusplay-client` share it. `ThreadTuning::from_config`
  reads `network.frame_thread_nice`, `network.rtkit_priority` and `network.send_timer_slack_us`;
  `apply(ThreadRole)` runs on the thread it tunes, as the first thing the thread body does, and
  returns the `PriorityError`s of the requests that failed. The caller adds their count to its
  `thread_priority_failures` counter and carries on: a refused request is never fatal.
- `ThreadRole::Frame` takes the nice value. `ThreadRole::TimerDriven` (the host's send, audio and
  microphone threads and the client's receive, audio and microphone threads, all of which sleep on
  short timeouts) also takes the timer slack (`PR_SET_TIMERSLACK`, 0 leaves the kernel's).
  `setpriority` on Linux applies to the calling thread, and threads spawned afterwards inherit it.
- `network.frame_thread_nice` 0 leaves the thread alone. When the nice request is refused (no
  `RLIMIT_NICE` headroom) and `network.rtkit_priority` is not 0, the thread asks rtkit
  (`org.freedesktop.RealtimeKit1` on the system bus, through `zbus`'s async proxy, each call driven
  to completion with `zbus::block_on`, since zbus 6 has no blocking API) for
  `MakeThreadRealtime` at that priority, clamped to rtkit's `MaxRealtimePriority`. rtkit refuses
  unless the process's `RLIMIT_RTTIME` is at most its `RTTimeUSecMax`, so the limit is lowered to
  it first (only when it is higher or unlimited). A thread that burns that much CPU without
  blocking is killed by the kernel, which is why `rtkit_priority` defaults to 0 and a decode
  thread on a slow software decoder is the case to watch.
- The rtkit connection is made per request and dropped after it; the D-Bus calls sit behind the
  `RealtimeService` trait so the fallback logic is tested with a fake, and the real bus is never
  contacted by a test.
- Each failure is its own `PriorityError` variant (`Nice`, `TimerSlack`, `RtkitUnreachable`,
  `RtkitLimits`, `RtkitNoRealtime`, `RealtimeTimeLimit`, `RtkitRefused`).
