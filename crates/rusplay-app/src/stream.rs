mod details;

use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use gpui::{
    Bounds, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable, FontWeight, Pixels,
    Subscription, Task, Window, canvas, div, prelude::*, px,
};
use parking_lot::{Condvar, Mutex};
use rusplay_client::{
    ActivePointer, BackendKind, ClientAction, ClientDisplay, ClientError, ClientStream,
    ClipboardState, Credentials, FocusClipboardHandle, LogicalArea, OverlayText, PeerRecord,
    StreamWindow, WindowLayout, announced_display,
};
use rusplay_config::{Config, InputConfig, PointerMode, ReleaseChord, VideoDecoderConfig};
use rusplay_core::{FrameRate, Resolution};

use crate::{
    clipboard::{self, FocusSlot},
    hosts::{HostEntry, HostId},
    notice::{Notice, error_chain},
    session::{self, SessionError},
    stats::{FrameTimings, JsonReporter, StatsSampler, StreamRates, TimingSink},
    theme,
    widgets::controls::{ButtonStyle, Tone, button, status_dot},
};

const POLL_INTERVAL: Duration = Duration::from_millis(50);
const DIVIDER_HEIGHT: f32 = 18.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    Paired {
        host: HostId,
        name: String,
        peer: PeerRecord,
    },
    Notice(Notice),
    Abandoned,
    Ended(Notice),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Connecting,
    Streaming,
    Abandoned,
    Ended,
}

struct Live {
    window: Option<StreamWindow>,
    stream: Option<ClientStream>,
    failure: Option<ClientError>,
    refresh: FrameRate,
    announced: Option<Resolution>,
    connect: CanvasConnect,
}

impl Live {
    fn apply_layout(&mut self, bounds: Bounds<Pixels>, scale: f32) {
        let area = LogicalArea {
            x: bounds.origin.x.as_f32(),
            y: bounds.origin.y.as_f32(),
            width: bounds.size.width.as_f32(),
            height: bounds.size.height.as_f32(),
        };
        let Some(wanted) = canvas_size(area, scale) else {
            return;
        };
        let Some(stream) = &self.stream else {
            self.offer_connect(wanted);
            return;
        };
        let Some(window) = &mut self.window else {
            return;
        };

        if self.announced != Some(wanted) {
            self.announced = Some(wanted);
            if let Ok(display) = announced_display(wanted, self.refresh, &window.color_target()) {
                stream.announce_display(display);
            }
        }

        let layout = stream
            .stream_format()
            .and_then(|format| WindowLayout::letterbox(area, scale, format.resolution));

        stream.set_target(layout.map(|layout| layout.pixels));
        if let Err(error) = window.set_layout(layout) {
            self.failure.get_or_insert(error);
        }
    }

    fn offer_connect(&mut self, wanted: Resolution) {
        if self.announced.is_some() {
            return;
        }

        let Some(volume) = self.window.as_ref().map(StreamWindow::color_target) else {
            return;
        };
        let Ok(display) = announced_display(wanted, self.refresh, &volume) else {
            return;
        };
        if self.connect.offer(display) {
            self.announced = Some(wanted);
        }
    }

    fn has_ended(&self) -> bool {
        self.failure.is_some()
            || self.stream.as_ref().is_some_and(ClientStream::is_finished)
            || self.window.as_ref().is_some_and(StreamWindow::is_finished)
    }

    fn stop(&mut self) -> Result<(), ClientError> {
        self.connect.cancel();
        let input = self
            .window
            .as_mut()
            .map_or(Ok(()), StreamWindow::stop_input);
        let stream = self.stream.take().map_or(Ok(()), ClientStream::stop);
        let window = self.window.take().map_or(Ok(()), StreamWindow::stop);

        match self.failure.take() {
            Some(failure) => Err(failure),
            None => stream.and(input).and(window),
        }
    }
}

pub struct StreamView {
    host: HostEntry,
    phase: Phase,
    live: Rc<RefCell<Live>>,
    format_generation: Option<u32>,
    input: InputConfig,
    decoder: VideoDecoderConfig,
    engaged: bool,
    pointer: Option<(PointerMode, ActivePointer)>,
    clipboard_slot: FocusSlot,
    clipboard: Option<FocusClipboardHandle>,
    show_stats: bool,
    stats_overlay: bool,
    requested_bitrate_kbps: u32,
    sampler: StatsSampler,
    json_reporter: Option<JsonReporter>,
    rates: Option<StreamRates>,
    screen: Screen,
    focus_handle: FocusHandle,
    tasks: Vec<Task<()>>,
    _activation: Subscription,
}

impl EventEmitter<StreamEvent> for StreamView {}

impl StreamView {
    pub fn new(
        host: HostEntry,
        credentials: Credentials,
        config: Config,
        stream_window: StreamWindow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let gpu = stream_window.gpu();
        let timings = Arc::new(FrameTimings::default());
        let sink = TimingSink::new(stream_window.sink(), Arc::clone(&timings));
        let address = host.address;
        let input = config.input;
        let decoder = config.video.decoder.clone();
        let stats_overlay = config.video.stats_overlay;
        let refresh = config.video.frame_rate;
        let requested_bitrate_kbps = config.video.bitrate_kbps;
        let json_reporter = JsonReporter::new(&config.network, Instant::now());
        let connect = CanvasConnect::new();
        let connect_wait = connect.waiter();

        let clipboard_slot = FocusSlot::default();
        let clipboard_opener = clipboard::session_opener(Arc::clone(&clipboard_slot));

        let connecting = cx.background_spawn(async move {
            let display = connect_wait.wait()?;

            Some(session::connect(
                config,
                address,
                &credentials,
                gpu,
                sink,
                Some(display),
                clipboard_opener,
            ))
        });
        let task = cx.spawn_in(window, async move |this, cx| {
            let Some(result) = connecting.await else {
                this.update_in(cx, |this, _, cx| this.connect_cancelled(cx))
                    .ok();
                return;
            };
            this.update_in(cx, |this, window, cx| this.connected(result, window, cx))
                .ok();
        });

        Self {
            host,
            phase: Phase::Connecting,
            live: Rc::new(RefCell::new(Live {
                window: Some(stream_window),
                stream: None,
                failure: None,
                refresh,
                announced: None,
                connect,
            })),
            format_generation: None,
            input,
            decoder,
            engaged: false,
            pointer: None,
            clipboard_slot,
            clipboard: None,
            show_stats: true,
            stats_overlay,
            requested_bitrate_kbps,
            sampler: StatsSampler::new(timings),
            json_reporter,
            rates: None,
            screen: Screen::from_window(window.is_fullscreen()),
            focus_handle: cx.focus_handle(),
            tasks: vec![task],
            _activation: cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.focus_gained(cx);
                } else {
                    this.release(cx);
                }
            }),
        }
    }

    pub fn disconnect(&mut self, cx: &mut Context<Self>) {
        if self.phase == Phase::Connecting {
            self.live.borrow_mut().stop().ok();
            self.phase = Phase::Abandoned;
            cx.emit(StreamEvent::Abandoned);
            return;
        }

        self.finish(cx);
    }

    pub fn is_engaged(&self) -> bool {
        self.engaged
    }

    fn release(&mut self, cx: &mut Context<Self>) {
        if let Some(window) = &self.live.borrow().window {
            window.release();
        }
        if self.engaged {
            self.engaged = false;
            cx.notify();
        }
    }

    fn cycle_pointer_mode(&mut self) {
        if let Some(window) = &self.live.borrow().window {
            window.cycle_pointer_mode();
        }
    }

    fn toggle_stats(&mut self, cx: &mut Context<Self>) {
        self.show_stats = !self.show_stats;
        self.sync_overlay();
        cx.notify();
    }

    fn sync_overlay(&self) {
        let hint = if self.screen.shows_hint() {
            fullscreen_hint(&self.input)
        } else {
            Vec::new()
        };
        let stats = (self.stats_overlay && self.show_stats)
            .then(|| self.rates.as_ref().map(StreamRates::overlay))
            .flatten();
        let lines: Vec<String> = hint
            .into_iter()
            .chain(stats.iter().flat_map(|text| text.lines().iter().cloned()))
            .collect();
        let text = (!lines.is_empty()).then(|| OverlayText::new(lines));

        if let Some(window) = &self.live.borrow().window {
            window.set_overlay(text);
        }
    }

    fn run_action(&mut self, action: ClientAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            ClientAction::ToggleFullscreen => window.toggle_fullscreen(),
            ClientAction::ExitFullscreen => {
                if window.is_fullscreen() {
                    window.toggle_fullscreen();
                }
            }
            ClientAction::ToggleStats => self.toggle_stats(cx),
            ClientAction::Disconnect => self.disconnect(cx),
        }
    }

    fn follow_fullscreen(&mut self, fullscreen: bool, now: Instant) {
        let hint = Duration::from_millis(u64::from(self.input.fullscreen_hint_ms));
        let previous = self.screen;
        self.screen = previous.follow(fullscreen, now, hint);

        if previous.shows_hint() != self.screen.shows_hint() {
            self.sync_overlay();
        }
    }

    fn connect_cancelled(&mut self, cx: &mut Context<Self>) {
        if self.phase == Phase::Ended || self.phase == Phase::Streaming {
            return;
        }

        self.live.borrow_mut().stop().ok();
        self.phase = Phase::Ended;
        cx.emit(StreamEvent::Ended(Notice::Info(format!(
            "Connection to {} cancelled",
            self.host.name
        ))));
    }

    fn connected(
        &mut self,
        result: Result<ClientStream, SessionError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.phase == Phase::Abandoned {
            if let Ok(stream) = result {
                self.report_pairing(&stream, cx);
                stream.stop().ok();
            }
            self.phase = Phase::Ended;
            cx.emit(StreamEvent::Ended(Notice::Info(format!(
                "Connection to {} cancelled",
                self.host.name
            ))));
            return;
        }
        if self.phase != Phase::Connecting {
            return;
        }

        let stream = match result {
            Ok(stream) => stream,
            Err(error) => {
                self.live.borrow_mut().stop().ok();
                self.phase = Phase::Ended;
                cx.emit(StreamEvent::Ended(Notice::Failure(format!(
                    "Could not connect to {}: {}",
                    self.host.name,
                    error_chain(&error)
                ))));
                return;
            }
        };

        self.report_pairing(&stream, cx);
        Self::report_clipboard(&stream, cx);
        if matches!(
            stream.clipboard(),
            ClipboardState::Active(BackendKind::Focus)
        ) {
            self.clipboard = self.clipboard_slot.lock().take();
        }

        let captured = {
            let mut live = self.live.borrow_mut();
            let captured = match &mut live.window {
                Some(window) => {
                    window.follow_cursor(&stream.cursor());
                    window.capture_input(&stream, &self.input)
                }
                None => Ok(()),
            };
            live.stream = Some(stream);
            captured
        };
        if let Err(error) = captured {
            cx.emit(StreamEvent::Notice(Notice::Failure(format!(
                "Streaming without input: {}",
                error_chain(&error)
            ))));
        }
        self.phase = Phase::Streaming;
        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                let running = this.update_in(cx, Self::poll).unwrap_or(false);
                if !running {
                    break;
                }
            }
        }));
        cx.notify();
    }

    fn report_clipboard(stream: &ClientStream, cx: &mut Context<Self>) {
        if let Some(text) = clipboard::notice_text(stream.clipboard()) {
            cx.emit(StreamEvent::Notice(Notice::Info(text)));
        }
    }

    fn focus_gained(&mut self, cx: &mut Context<Self>) {
        let Some(handle) = &mut self.clipboard else {
            return;
        };
        if !handle.reads_on_focus() {
            return;
        }

        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            handle.offer(text);
        }
    }

    fn apply_incoming_clipboard(&mut self, cx: &mut Context<Self>) {
        let incoming = self
            .clipboard
            .as_mut()
            .and_then(FocusClipboardHandle::take_incoming);

        if let Some(text) = incoming {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn report_pairing(&self, stream: &ClientStream, cx: &mut Context<Self>) {
        let Some(secret) = stream.paired_secret() else {
            return;
        };

        match PeerRecord::new(&self.host.name, secret.clone()) {
            Ok(peer) => cx.emit(StreamEvent::Paired {
                host: self.host.id,
                name: self.host.name.clone(),
                peer,
            }),
            Err(error) => cx.emit(StreamEvent::Notice(Notice::Failure(format!(
                "Paired with {}, but the pairing cannot be remembered: {error}",
                self.host.name
            )))),
        }
    }

    fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.phase != Phase::Streaming {
            return false;
        }
        if self.live.borrow().has_ended() {
            self.finish(cx);
            return false;
        }

        self.apply_incoming_clipboard(cx);

        let actions = self
            .live
            .borrow()
            .window
            .as_ref()
            .map_or_else(Vec::new, StreamWindow::take_actions);
        for action in actions {
            self.run_action(action, window, cx);
        }
        if self.phase != Phase::Streaming {
            return false;
        }

        self.follow_fullscreen(window.is_fullscreen(), Instant::now());

        let generation = self
            .live
            .borrow()
            .stream
            .as_ref()
            .and_then(ClientStream::stream_format)
            .map(|format| format.generation);
        if generation != self.format_generation {
            self.format_generation = generation;
            cx.notify();
        }

        let engaged = self
            .live
            .borrow()
            .window
            .as_ref()
            .is_some_and(StreamWindow::is_engaged);
        if engaged != self.engaged {
            self.engaged = engaged;
            cx.notify();
        }

        let pointer = self
            .live
            .borrow()
            .window
            .as_ref()
            .and_then(|window| window.pointer_mode().zip(window.active_pointer()));
        if pointer != self.pointer {
            self.pointer = pointer;
            cx.notify();
        }

        let stats = self.live.borrow().stream.as_ref().map(ClientStream::stats);
        if let (Some(reporter), Some(stats)) = (&mut self.json_reporter, &stats) {
            reporter.report_if_due(Instant::now(), stats);
        }
        if let Some(rates) = stats
            .as_ref()
            .and_then(|stats| self.sampler.sample(Instant::now(), stats))
        {
            let presentation = self
                .live
                .borrow()
                .window
                .as_ref()
                .map(StreamWindow::presentation);
            let rates = match presentation {
                Some(presentation) => rates.with_presentation(&presentation),
                None => rates,
            };
            self.rates = Some(rates);
            self.sync_overlay();
            cx.notify();
        }

        true
    }

    fn finish(&mut self, cx: &mut Context<Self>) {
        if self.phase == Phase::Ended {
            return;
        }

        let last = self.live.borrow().stream.as_ref().map(ClientStream::stats);
        if let (Some(_), Some(stats)) = (&self.json_reporter, &last) {
            JsonReporter::report_final(stats);
        }

        self.clipboard = None;

        let result = self.live.borrow_mut().stop();
        self.phase = Phase::Ended;

        let notice = match result {
            Ok(()) => Notice::Info(format!("Session with {} ended", self.host.name)),
            Err(error) => Notice::Failure(format!(
                "Session with {} ended: {}",
                self.host.name,
                error_chain(&error)
            )),
        };
        cx.emit(StreamEvent::Ended(notice));
    }

    const fn tone(&self) -> Tone {
        match self.phase {
            Phase::Connecting => Tone::Warning,
            Phase::Streaming if self.engaged => Tone::Accent,
            Phase::Streaming => Tone::Success,
            Phase::Abandoned | Phase::Ended => Tone::Neutral,
        }
    }

    fn status(&self) -> String {
        match self.phase {
            Phase::Connecting => format!("Connecting to {}", self.host.name),
            Phase::Streaming if self.engaged => format!(
                "Input captured by {}. Press {}{} to release it{}",
                self.host.name,
                chord_label(self.input.release_chord),
                escape_hint(&self.input),
                hotkey_hint(&self.input)
            ),
            Phase::Streaming => format!(
                "Streaming from {}. Click the picture or press {} to capture input",
                self.host.name,
                chord_label(self.input.release_chord)
            ),
            Phase::Abandoned | Phase::Ended => format!("Session with {} ended", self.host.name),
        }
    }
}

fn area_size(area: LogicalArea, scale: f32) -> Resolution {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let device = |logical: f32| (logical * scale).round().max(1.0) as u32;

    Resolution::new(device(area.width), device(area.height))
}

fn canvas_size(area: LogicalArea, scale: f32) -> Option<Resolution> {
    if area.width < 1.0 || area.height < 1.0 {
        return None;
    }

    Some(area_size(area, scale))
}

struct HeldOnce<T> {
    value: Mutex<Option<T>>,
    ready: Condvar,
    cancelled: AtomicBool,
}

impl<T> HeldOnce<T> {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            value: Mutex::new(None),
            ready: Condvar::new(),
            cancelled: AtomicBool::new(false),
        })
    }

    fn offer(&self, value: T) -> bool {
        let mut slot = self.value.lock();
        if self.cancelled.load(Ordering::Relaxed) || slot.is_some() {
            return false;
        }

        *slot = Some(value);
        self.ready.notify_one();
        true
    }

    fn cancel(&self) {
        let _slot = self.value.lock();
        self.cancelled.store(true, Ordering::Relaxed);
        self.ready.notify_one();
    }

    fn wait(&self) -> Option<T> {
        let mut slot = self.value.lock();
        while slot.is_none() && !self.cancelled.load(Ordering::Relaxed) {
            self.ready.wait(&mut slot);
        }
        if self.cancelled.load(Ordering::Relaxed) {
            None
        } else {
            slot.take()
        }
    }
}

struct CanvasConnect {
    gate: Arc<HeldOnce<ClientDisplay>>,
}

impl CanvasConnect {
    fn new() -> Self {
        Self {
            gate: HeldOnce::new(),
        }
    }

    fn waiter(&self) -> Arc<HeldOnce<ClientDisplay>> {
        Arc::clone(&self.gate)
    }

    fn offer(&self, display: ClientDisplay) -> bool {
        self.gate.offer(display)
    }

    fn cancel(&self) {
        self.gate.cancel();
    }
}

impl Drop for CanvasConnect {
    fn drop(&mut self) {
        self.gate.cancel();
    }
}

fn hotkey_hint(input: &InputConfig) -> String {
    if !input.hotkeys {
        return String::new();
    }

    format!(
        ", {}+{:?} fullscreen, {:?} statistics, {:?} disconnect, {:?} pointer",
        chord_label(input.hotkey_chord),
        input.hotkey_fullscreen,
        input.hotkey_stats,
        input.hotkey_disconnect,
        input.hotkey_pointer_mode
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    Windowed,
    Fullscreen { hint_until: Option<Instant> },
}

impl Screen {
    fn from_window(fullscreen: bool) -> Self {
        if fullscreen {
            Self::Fullscreen { hint_until: None }
        } else {
            Self::Windowed
        }
    }

    fn follow(self, fullscreen: bool, now: Instant, hint: Duration) -> Self {
        match (self, fullscreen) {
            (_, false) => Self::Windowed,
            (Self::Windowed, true) => Self::Fullscreen {
                hint_until: (!hint.is_zero()).then(|| now + hint),
            },
            (Self::Fullscreen { hint_until }, true) => Self::Fullscreen {
                hint_until: hint_until.filter(|until| now < *until),
            },
        }
    }

    fn shows_hint(self) -> bool {
        matches!(
            self,
            Self::Fullscreen {
                hint_until: Some(_)
            }
        )
    }
}

fn escape_hint(input: &InputConfig) -> &'static str {
    if input.escape_hold_ms == 0 {
        ""
    } else {
        " or hold Esc"
    }
}

fn fullscreen_hint(input: &InputConfig) -> Vec<String> {
    let mut lines = Vec::new();

    if input.escape_hold_ms != 0 {
        lines.push("HOLD ESC TO EXIT FULLSCREEN".to_owned());
    }
    if input.hotkeys {
        lines.push(format!(
            "{}+{:?} TOGGLES FULLSCREEN",
            chord_label(input.hotkey_chord),
            input.hotkey_fullscreen
        ));
    }
    lines.push(format!(
        "{} RELEASES INPUT",
        chord_label(input.release_chord)
    ));

    lines
}

fn pointer_label(mode: PointerMode, active: ActivePointer) -> String {
    let active = match active {
        ActivePointer::Absolute => "absolute",
        ActivePointer::Relative => "relative",
    };

    match mode {
        PointerMode::Absolute | PointerMode::Relative => format!("Pointer: {active}"),
        PointerMode::Auto => format!("Pointer: auto ({active})"),
    }
}

fn chord_label(chord: ReleaseChord) -> &'static str {
    match chord {
        ReleaseChord::CtrlAlt => "Ctrl+Alt",
        ReleaseChord::CtrlShift => "Ctrl+Shift",
        ReleaseChord::AltShift => "Alt+Shift",
        ReleaseChord::CtrlAltShift => "Ctrl+Alt+Shift",
    }
}

impl Focusable for StreamView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl StreamView {
    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap_4()
            .px_4()
            .py_2p5()
            .border_b_1()
            .border_color(theme::border())
            .bg(theme::chrome())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .min_w_0()
                    .child(status_dot(self.tone()))
                    .child(
                        div().flex().flex_col().gap_0p5().min_w_0().child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme::text())
                                .child(self.status()),
                        ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .children(self.pointer.map(|(mode, active)| {
                        button(
                            "pointer-mode",
                            pointer_label(mode, active),
                            ButtonStyle::Quiet,
                            true,
                        )
                        .on_click(cx.listener(|this, _, _, _| this.cycle_pointer_mode()))
                    }))
                    .child(
                        button(
                            "stats",
                            if self.show_stats {
                                "Hide details"
                            } else {
                                "Connection details"
                            },
                            ButtonStyle::Quiet,
                            true,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_stats(cx))),
                    )
                    .child(
                        button("fullscreen", "Fullscreen", ButtonStyle::Quiet, true)
                            .on_click(cx.listener(|_, _, window, _| window.toggle_fullscreen())),
                    )
                    .child(
                        div()
                            .w(px(1.0))
                            .h(px(DIVIDER_HEIGHT))
                            .mx_1()
                            .bg(theme::border()),
                    )
                    .child(
                        button("disconnect", "Disconnect", ButtonStyle::Danger, true)
                            .on_click(cx.listener(|this, _, _, cx| this.disconnect(cx))),
                    ),
            )
    }
}

impl Render for StreamView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let live = Rc::clone(&self.live);
        let picture = canvas(
            move |bounds, window, _| {
                live.borrow_mut()
                    .apply_layout(bounds, window.scale_factor());
            },
            |_, (), _, _| {},
        )
        .size_full();

        let header = self.render_header(cx);

        div()
            .id("stream")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .children((!window.is_fullscreen()).then_some(header))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .bg(theme::backdrop())
                            .child(picture),
                    )
                    .children(
                        (self.show_stats && !window.is_fullscreen())
                            .then(|| self.render_details(cx)),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_announced_area_is_the_canvas_in_device_pixels_and_never_empty() {
        let area = |width, height| LogicalArea {
            x: 0.0,
            y: 40.0,
            width,
            height,
        };

        assert_eq!(
            canvas_size(area(1280.0, 680.0), 1.5),
            Some(Resolution::new(1920, 1020))
        );
        assert_eq!(canvas_size(area(0.0, 0.0), 2.0), None);
        assert_eq!(canvas_size(area(0.0, -3.0), 2.0), None);
    }

    #[test]
    fn the_connect_gate_keeps_the_first_offer_and_a_cancel_wakes_the_waiter() {
        let gate = HeldOnce::new();

        assert!(gate.offer(Resolution::new(1280, 720)));
        assert!(!gate.offer(Resolution::new(800, 600)));
        assert_eq!(gate.wait(), Some(Resolution::new(1280, 720)));

        let gate = HeldOnce::new();
        let waiting = Arc::clone(&gate);
        let thread = std::thread::spawn(move || waiting.wait());
        gate.cancel();

        assert_eq!(thread.join().unwrap(), None);
        assert!(!gate.offer(Resolution::new(640, 360)));
    }

    #[test]
    fn the_pointer_label_names_the_active_pointer_and_whether_auto_chose_it() {
        assert_eq!(
            pointer_label(PointerMode::Relative, ActivePointer::Relative),
            "Pointer: relative"
        );
        assert_eq!(
            pointer_label(PointerMode::Auto, ActivePointer::Absolute),
            "Pointer: auto (absolute)"
        );
    }

    #[test]
    fn the_hotkey_hint_names_the_chord_and_each_key_or_vanishes_when_off() {
        let on = InputConfig::default();
        let off = InputConfig {
            hotkeys: false,
            ..InputConfig::default()
        };

        assert_eq!(
            hotkey_hint(&on),
            ", Ctrl+Alt+F fullscreen, S statistics, D disconnect, P pointer"
        );
        assert_eq!(hotkey_hint(&off), "");
    }

    #[test]
    fn the_fullscreen_hint_names_every_way_out_that_is_turned_on() {
        let on = InputConfig::default();
        let bare = InputConfig {
            hotkeys: false,
            escape_hold_ms: 0,
            ..InputConfig::default()
        };

        assert_eq!(
            fullscreen_hint(&on),
            [
                "HOLD ESC TO EXIT FULLSCREEN",
                "Ctrl+Alt+F TOGGLES FULLSCREEN",
                "Ctrl+Alt+Shift RELEASES INPUT"
            ]
        );
        assert_eq!(fullscreen_hint(&bare), ["Ctrl+Alt+Shift RELEASES INPUT"]);
        assert_eq!(escape_hint(&on), " or hold Esc");
        assert_eq!(escape_hint(&bare), "");
    }

    #[test]
    fn the_hint_shows_from_entering_fullscreen_until_it_expires_or_the_window_leaves() {
        let start = Instant::now();
        let hint = Duration::from_secs(3);

        let entered = Screen::Windowed.follow(true, start, hint);
        let showing = entered.follow(true, start + Duration::from_millis(2999), hint);
        let expired = showing.follow(true, start + hint, hint);
        let left = entered.follow(false, start, hint);
        let silent = Screen::Windowed.follow(true, start, Duration::ZERO);
        let already = Screen::from_window(true).follow(true, start, hint);

        assert!(entered.shows_hint() && showing.shows_hint());
        assert!(!expired.shows_hint());
        assert_eq!(left, Screen::Windowed);
        assert!(!silent.shows_hint() && !already.shows_hint());
    }

    #[test]
    fn every_release_chord_has_a_label_naming_its_keys() {
        assert_eq!(chord_label(ReleaseChord::default()), "Ctrl+Alt+Shift");
        assert_eq!(chord_label(ReleaseChord::CtrlAlt), "Ctrl+Alt");
        assert_eq!(chord_label(ReleaseChord::CtrlShift), "Ctrl+Shift");
        assert_eq!(chord_label(ReleaseChord::AltShift), "Alt+Shift");
    }
}
