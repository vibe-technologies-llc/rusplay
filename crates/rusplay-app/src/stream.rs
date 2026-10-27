use std::{
    cell::RefCell,
    net::SocketAddr,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    Bounds, Context, EventEmitter, FocusHandle, Focusable, Pixels, Subscription, Task, Window,
    canvas, div, prelude::*,
};
use rusplay_client::{
    ActivePointer, ClientAction, ClientError, ClientStream, Credentials, LogicalArea, PeerRecord,
    StreamWindow, WindowLayout,
};
use rusplay_config::{Config, InputConfig, PointerMode, ReleaseChord};

use crate::{
    hosts::HostEntry,
    notice::{Notice, error_chain},
    session::{self, SessionError},
    stats::{FrameTimings, StatsSampler, StreamRates, TimingSink},
    theme,
    widgets::controls::{ButtonStyle, button},
};

const POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    Paired {
        address: SocketAddr,
        peer: PeerRecord,
    },
    Notice(Notice),
    Ended(Notice),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Connecting,
    Streaming,
    Ended,
}

#[derive(Default)]
struct Live {
    window: Option<StreamWindow>,
    stream: Option<ClientStream>,
    failure: Option<ClientError>,
}

impl Live {
    fn apply_layout(&mut self, bounds: Bounds<Pixels>, scale: f32) {
        let (Some(stream), Some(window)) = (&self.stream, &mut self.window) else {
            return;
        };

        let area = LogicalArea {
            x: bounds.origin.x.as_f32(),
            y: bounds.origin.y.as_f32(),
            width: bounds.size.width.as_f32(),
            height: bounds.size.height.as_f32(),
        };
        let layout = stream
            .stream_format()
            .and_then(|format| WindowLayout::letterbox(area, scale, format.resolution));

        stream.set_target(layout.map(|layout| layout.pixels));
        if let Err(error) = window.set_layout(layout) {
            self.failure.get_or_insert(error);
        }
    }

    fn has_ended(&self) -> bool {
        self.failure.is_some()
            || self.stream.as_ref().is_some_and(ClientStream::is_finished)
            || self.window.as_ref().is_some_and(StreamWindow::is_finished)
    }

    fn stop(&mut self) -> Result<(), ClientError> {
        let stream = self.stream.take().map_or(Ok(()), ClientStream::stop);
        let window = self.window.take().map_or(Ok(()), StreamWindow::stop);

        match self.failure.take() {
            Some(failure) => Err(failure),
            None => stream.and(window),
        }
    }
}

pub struct StreamView {
    host: HostEntry,
    phase: Phase,
    live: Rc<RefCell<Live>>,
    format_generation: Option<u32>,
    input: InputConfig,
    engaged: bool,
    pointer: Option<(PointerMode, ActivePointer)>,
    show_stats: bool,
    sampler: StatsSampler,
    rates: Option<StreamRates>,
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

        let connecting = cx.background_spawn(async move {
            session::connect(config, address, &credentials, gpu, sink)
        });
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = connecting.await;
            this.update_in(cx, |this, window, cx| this.connected(result, window, cx))
                .ok();
        });

        Self {
            host,
            phase: Phase::Connecting,
            live: Rc::new(RefCell::new(Live {
                window: Some(stream_window),
                ..Live::default()
            })),
            format_generation: None,
            input,
            engaged: false,
            pointer: None,
            show_stats: true,
            sampler: StatsSampler::new(timings),
            rates: None,
            focus_handle: cx.focus_handle(),
            tasks: vec![task],
            _activation: cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() {
                    this.release(cx);
                }
            }),
        }
    }

    pub fn disconnect(&mut self, cx: &mut Context<Self>) {
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
        cx.notify();
    }

    fn run_action(&mut self, action: ClientAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            ClientAction::ToggleFullscreen => window.toggle_fullscreen(),
            ClientAction::ToggleStats => self.toggle_stats(cx),
            ClientAction::Disconnect => self.disconnect(cx),
        }
    }

    fn connected(
        &mut self,
        result: Result<ClientStream, SessionError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

        if let Some(secret) = stream.paired_secret() {
            match PeerRecord::new(&self.host.name, secret.clone()) {
                Ok(peer) => cx.emit(StreamEvent::Paired {
                    address: self.host.address,
                    peer,
                }),
                Err(error) => cx.emit(StreamEvent::Notice(Notice::Failure(format!(
                    "Paired with {}, but the pairing cannot be remembered: {error}",
                    self.host.name
                )))),
            }
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

    fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.phase != Phase::Streaming {
            return false;
        }
        if self.live.borrow().has_ended() {
            self.finish(cx);
            return false;
        }

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
        if let Some(rates) = stats
            .as_ref()
            .and_then(|stats| self.sampler.sample(Instant::now(), stats))
        {
            self.rates = Some(rates);
            cx.notify();
        }

        true
    }

    fn finish(&mut self, cx: &mut Context<Self>) {
        if self.phase == Phase::Ended {
            return;
        }

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

    fn status(&self) -> String {
        match self.phase {
            Phase::Connecting => format!("Connecting to {}", self.host.name),
            Phase::Streaming if self.engaged => format!(
                "Input captured by {}. Press {} to release it{}",
                self.host.name,
                chord_label(self.input.release_chord),
                hotkey_hint(&self.input)
            ),
            Phase::Streaming => format!(
                "Streaming from {}. Click the picture or press {} to capture input",
                self.host.name,
                chord_label(self.input.release_chord)
            ),
            Phase::Ended => format!("Session with {} ended", self.host.name),
        }
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

        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(theme::border())
            .bg(theme::surface())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_color(theme::text()).child(self.status()))
                    .children(self.rates.filter(|_| self.show_stats).map(|rates| {
                        div()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child(rates.summary())
                    })),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
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
                                "Hide stats"
                            } else {
                                "Show stats"
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
                        button("disconnect", "Disconnect", ButtonStyle::Secondary, true)
                            .on_click(cx.listener(|this, _, _, cx| this.disconnect(cx))),
                    ),
            );

        div()
            .id("stream")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .children((!window.is_fullscreen()).then_some(header))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .bg(theme::backdrop())
                    .child(picture),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn every_release_chord_has_a_label_naming_its_keys() {
        assert_eq!(chord_label(ReleaseChord::default()), "Ctrl+Alt+Shift");
        assert_eq!(chord_label(ReleaseChord::CtrlAlt), "Ctrl+Alt");
        assert_eq!(chord_label(ReleaseChord::CtrlShift), "Ctrl+Shift");
        assert_eq!(chord_label(ReleaseChord::AltShift), "Alt+Shift");
    }
}
