# Audit

Deep read of the workspace at `10fb801` (`master`, 2026-11-07). This file tracks the remaining
findings; completed work is removed as fixes land. It does not change behaviour, and it does not repeat items already
listed in [BUGS.md](BUGS.md), [FEATURES.md](FEATURES.md) or
[PERFORMANCE.md](PERFORMANCE.md).

`cargo audit` (database 2026-11-06) reported no critical or high advisories and
no yanked crates in the workspace lockfile or in `fuzz/Cargo.lock`. Crate
dependency direction holds: media crates do not depend on `net`, and
`rusplay-encode` and `rusplay-decode` meet only as a dev-dependency.

## Scope

About 158k lines of Rust under `crates/`, plus the rules in `.claude/rules/`.
Passes covered correctness (decode, encode, host, client, net, audio),
security of the trust boundaries, the steady-state frame path, tests on the
critical paths, structure, dependencies, docs drift and direction.

Not audited, or only sampled:

- A live GPU session, a nested compositor, or a second machine. Hardware tests
  were not run for this report.
- `vendor/rav1d` beyond confirming that crates.io 1.1.0 still has the three
  local fixes (send-path unwrap, CDEF padding, picture-unref leak).
- The fuzz corpus and shader numerics. Colour and scale already have GPU tests
  described in the rules.
- gpui widget behaviour, and a line-coverage measurement of the suite.

## Findings

Ordered by leverage: impact against effort, discounted by confidence and by
how easily the fix itself goes wrong.

| # | Finding | Category | Impact | Effort | Risk | Confidence |
|---|---|---|---|---|---|---|
| 15 | Several recent fixes have tests that would still pass if the fix were reverted | Tests | FFmpeg retention, conversion fallback and caller coverage remain | M | LOW | HIGH |

### 15. Lock the remaining regression properties

Decode-loop failure skipping, eager decoder opening, successful handoff fallback,
and preservation of the input portal token now have stronger regression tests.
The conversion failure fallback also increments `handoff_disabled`.

- **FFmpeg reference loss.** `crates/rusplay-decode/tests/ffmpeg.rs` still submits an
  IDR after the missing-reference error. That passes even if flushing is restored.
  A probe submitting the skipped dependent packet after the error also failed
  with the current implementation: this fixture does not demonstrate reference
  retention. Build a sequence where an error leaves a reference usable and assert
  the dependent picture succeeds before replacing this test.
- Exercise Esc-hold through production `Session::tick` and apply `ExitFullscreen`
  with both fullscreen and windowed state.

## Direction

These are options, not defects. The roadmap already lists the unfinished hardware ports, virtual-output live checks, pacing, clipboard live checks and long-term reference work. They are not repeated here.

**Discovery past the default IPv4 route.** `networking.md` states that discovery is IPv4 only, that limited broadcast leaves through the default route, and that the multicast TTL is 1, so another interface or VLAN is invisible. `discovery_group` is an `Ipv4Addr`, and `docs/FEATURES.md` has no item for this. Typing an address still works. A design spike for one extra query path (IPv6 link-local, or IPv4 multicast on each up interface) is the proportional next step. Do not change pairing. Effort is coarse and large. Confidence that the limit is real and unscheduled is high.

**Host JSON stats from the in-app host, or stop offering the flag there.** `network.stats_json` is a shared settings row. The app writes client lines only, and `ui.md` says that is the only stdout besides `--version`. `rusplay-hostd` writes the same shape for `HostStats`. Someone hosting from the app cannot diff host JSON the way the daemon plus the app can. Saving the flag in `host.toml` does nothing until the daemon is used. Either the host page prints `HostStats` with a distinct `event`, or the flag is daemon-only in the form. Two writers on one stdout will interleave if the app is also the client. Effort is small to medium. Confidence is high.

## Considered and rejected

- **Unknown control tags decode as `Goodbye`.** They do not. The length table returns `UnknownControlTag`. The trailing `_ => Goodbye` is `TAG_GOODBYE`, which is in that table and not named again. `every_message()` is a fixed array of 16 and already omits `Display`, which has its own test. Worth closing when the next message is added, not on its own.
- **Move ashpd to 0.14 / zbus 6.** `cargo audit` reports the zvariant 5 line unmaintained, with no critical or high advisory. ashpd 0.14 depends on zbus 6. gpui still declares ashpd 0.13. Moving one of them puts two D-Bus stacks in the binary and breaks the one-stack rule. Not worth doing until the gpui rev moves.
- **Take spake2 0.5 to drop the second `sha2` / `hkdf` stack.** The only newer release is `0.5.0-pre.0`. The pairing transcript would change under a crate that is not a stable release. No advisory on either stack.
- **A general codec dump, or a third headless client binary.** The HEVC dump exists for one open import bug. A supported client binary would drift from the app on present, input and clipboard. The library already runs the loopback tests.
- **`peers.host_name` allows a control character that discovery rejects.** `DiscoveryResponder::bind` fails closed through `to_datagram`, so the host does not start. `client_name` already rejects control characters. A field error would be clearer. The host does not announce a bad name.
- **rav1d `EAGAIN` on both send and get.** `software.rs:125` breaks and returns `Ok(None)` if `dav1d_get_picture` also returns `WouldBlock`, and the input is dropped. `max_frame_delay` is 1. Whether that pair happens was not shown. Investigate before changing it: if it does, the access unit is discarded and `decode_errors` stays zero.
- **Per-frame `vaCreateBuffer` / `vaDestroyBuffer`, and the DMA-BUF import cache's FIFO eviction.** Both are plausible steady-state costs. Some drivers treat a VA buffer as single-use until it is destroyed, and the AV1 surface-pool high-water mark was not measured against the 16-entry cache. Not worth a change until a profile says they are on the critical path.
- **Missing CI.** Not a defect for this repository.
