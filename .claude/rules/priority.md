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
- `ThreadRole::Frame` takes the nice value. `ThreadRole::TimerDriven`, for a thread that sleeps
  toward a deadline on short timeouts, also takes the timer slack (`PR_SET_TIMERSLACK`, 0 leaves
  the kernel's). `setpriority` on Linux applies to the calling thread, and threads spawned
  afterwards inherit it.
- Every thread a frame or an input event crosses is tuned, because on a saturated machine (a game
  on every core of the host, a software decoder on the client) a wakeup at the default nice
  waits behind the tuned threads and the game:

  | Side | `Frame` | `TimerDriven` | Default |
  |---|---|---|---|
  | Host | encode, control, the PipeWire capture loop (`rusplay-pipewire`), the injector (`rusplay-host-injector`) | capture (it sleeps toward the frame-rate cap's slot), send, audio, microphone, input (its 4 ms service) | cursor, the service loop |
  | Client | decode, present, the Wayland input capture (`rusplay-wayland-input`), the window presenter with `video.present_pacing = "off"` | receive, audio, microphone, input (the repeat and snapshot deadlines), gamepad (5 ms poll), the window presenter in `latency` and `smooth` (it sleeps until a paced frame is due) | the output follower |

  The stage threads take their role from `thread_role(Stage)` in each crate's `session.rs`. A
  thread one of these spawns inherits its nice value (the host's quality thread from encode,
  rav1d's pool from decode).
- `rusplay-capture` and `rusplay-input` sit below `rusplay-priority` in the dependency
  direction, so their threads are tuned by a thread-start hook the composing crate fills:
  `PipeWireCapture::connect_with_thread_start` / `from_portal_with_thread_start` and
  `WaylandCapture::attach_with_thread_start` take an `FnOnce() + Send` that runs first on the
  new thread (`spawn_worker`, `spawn_capture`, each unit tested for the order and the thread).
- A thread that outlives a session cannot add to that session's counter when it starts. On the
  host the PipeWire loop and the injector keep a `TuningRefusals` tally on the `CaptureFeed` and
  the `InputFeed`, and every `HostStream` adds both to its `thread_priority_failures` when it
  starts. On the client the window presenter is spawned before the stream exists: it tallies on
  the window, `StreamWindow::attach` waits for that first step, and `ClientSession::connect`
  adds `FrameSink::thread_priority_failures` (0 by default, the tally for `WindowSink`) to the
  stream's counter. The Wayland input capture starts with the stream in hand and counts
  straight into it (`ClientStream::thread_start`).
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
