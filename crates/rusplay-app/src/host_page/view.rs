use std::time::{Duration, Instant};

use gpui::{Context, EventEmitter, FontWeight, Task, Window, div, prelude::*, px};
use rusplay_config::{ConfigEnvironment, ConfigRole};

use crate::{
    host_page::model::{HostModel, Phase, client_summary, grouped_pin},
    hosting::{self, HostEvent, HostHandle, HostingError, PairedClient, PairedClients, PeerId},
    notice::{Notice, NoticeRaised, error_chain},
    theme,
    widgets::controls::{ButtonStyle, Tone, badge, button, card, card_heading, status_dot, toggle},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostLaunchPreference(pub bool);

const POLL_INTERVAL: Duration = Duration::from_millis(250);
const SIDE_PANEL_WIDTH: f32 = 330.0;
const PIN_SIZE: f32 = 44.0;
const ROW_LABEL_WIDTH: f32 = 120.0;

pub struct HostPageView {
    environment: ConfigEnvironment,
    launch_on_start: bool,
    model: HostModel,
    handle: Option<HostHandle>,
    clients: Option<PairedClients>,
    clients_error: Option<String>,
    paired: Vec<PairedClient>,
    restart_when_stopped: bool,
    choose_display_on_start: bool,
    transition: Option<Task<()>>,
    _poller: Task<()>,
}

impl EventEmitter<NoticeRaised> for HostPageView {}
impl EventEmitter<HostLaunchPreference> for HostPageView {}

impl HostPageView {
    pub fn new(environment: ConfigEnvironment, cx: &mut Context<Self>) -> Self {
        let poller = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                if this.update(cx, Self::poll).is_err() {
                    break;
                }
            }
        });

        let mut view = Self {
            environment,
            launch_on_start: false,
            model: HostModel::default(),
            handle: None,
            clients: None,
            clients_error: None,
            paired: Vec::new(),
            restart_when_stopped: false,
            choose_display_on_start: false,
            transition: None,
            _poller: poller,
        };
        view.reopen_clients();
        view.reload_launch_preference();

        view
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.reload_launch_preference();
        if self.model.phase() == Phase::Stopped {
            self.reopen_clients();
        }
        cx.notify();
    }

    pub fn start_if_requested(&mut self, cx: &mut Context<Self>) {
        if self.launch_on_start {
            self.start(cx);
        }
    }

    pub fn shutdown(&mut self) {
        self.restart_when_stopped = false;
        if let Some(handle) = self.handle.take() {
            handle.stop();
            self.model.stopped();
        }
    }

    pub fn apply_saved_settings(&mut self, cx: &mut Context<Self>) {
        self.reload_launch_preference();
        if !self.model.restarts_for_saved_settings() {
            self.refresh(cx);
            return;
        }

        self.restart_when_stopped = true;
        if self.model.phase() == Phase::Running {
            self.stop(cx);
        }
        cx.emit(NoticeRaised(Notice::Info(
            "Restarting the host so the saved settings apply".to_owned(),
        )));
        cx.notify();
    }

    fn reload_launch_preference(&mut self) {
        self.launch_on_start = hosting::load_host_config(&self.environment)
            .is_ok_and(|config| config.peers.host_on_launch);
    }

    fn set_launch_on_start(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let mut config = match hosting::load_host_config(&self.environment) {
            Ok(config) => config,
            Err(error) => {
                cx.emit(NoticeRaised(Notice::Failure(format!(
                    "Could not update hosting on launch: {}",
                    error_chain(&error)
                ))));
                return;
            }
        };
        config.peers.host_on_launch = enabled;
        let written = self
            .environment
            .config_path(ConfigRole::Host)
            .and_then(|path| config.save(&path));

        match written {
            Ok(()) => {
                self.launch_on_start = enabled;
                cx.emit(HostLaunchPreference(enabled));
            }
            Err(error) => cx.emit(NoticeRaised(Notice::Failure(format!(
                "Could not update hosting on launch: {}",
                error_chain(&error)
            )))),
        }

        cx.notify();
    }

    fn reopen_clients(&mut self) {
        let opened = hosting::load_host_config(&self.environment)
            .map_err(HostingError::from)
            .and_then(|config| PairedClients::open(&config));

        match opened {
            Ok(clients) => {
                self.clients_error = clients.exposure();
                self.clients = Some(clients);
                self.reload_paired();
            }
            Err(error) => {
                self.clients_error = Some(format!(
                    "Paired clients are unavailable: {}",
                    error_chain(&error)
                ));
                self.clients = None;
                self.paired.clear();
            }
        }
    }

    fn reload_paired(&mut self) -> bool {
        let Some(clients) = &self.clients else {
            return false;
        };

        match clients.list() {
            Ok(list) if list != self.paired => {
                self.paired = list;
                true
            }
            Ok(_) => false,
            Err(error) => {
                let message = format!("Paired clients are unavailable: {}", error_chain(&error));
                let changed = self.clients_error.as_deref() != Some(&message);
                self.clients_error = Some(message);
                changed
            }
        }
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        if !self.model.begin_start() {
            return;
        }

        let environment = self.environment.clone();
        let choose_display = std::mem::take(&mut self.choose_display_on_start);
        let starting = cx.background_spawn(async move {
            if choose_display {
                hosting::start_choosing_display(&environment)
            } else {
                hosting::start(&environment)
            }
        });
        self.transition = Some(cx.spawn(async move |this, cx| {
            let result = starting.await;
            this.update(cx, |this, cx| this.started(result, cx)).ok();
        }));
        cx.notify();
    }

    fn started(
        &mut self,
        result: Result<(HostHandle, PairedClients), HostingError>,
        cx: &mut Context<Self>,
    ) {
        self.transition = None;

        match result {
            Ok((handle, clients)) => {
                self.model.started(handle.address());
                self.handle = Some(handle);
                self.clients_error = clients.exposure();
                self.clients = Some(clients);
                self.reload_paired();
                if self.restart_when_stopped {
                    self.stop(cx);
                    cx.notify();
                    return;
                }
                cx.emit(NoticeRaised(Notice::Info(format!(
                    "Hosting on port {}",
                    self.model.address().map_or(0, |address| address.port())
                ))));
            }
            Err(error) => {
                self.restart_when_stopped = false;
                self.model.start_failed();
                cx.emit(NoticeRaised(Notice::Failure(format!(
                    "Could not start the host: {}",
                    error_chain(&error)
                ))));
            }
        }

        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        if !self.model.begin_stop() {
            self.handle = Some(handle);
            return;
        }

        let stopping = cx.background_spawn(async move { handle.stop() });
        self.transition = Some(cx.spawn(async move |this, cx| {
            let events = stopping.await;
            this.update(cx, |this, cx| this.stopped(&events, cx)).ok();
        }));
        cx.notify();
    }

    fn stopped(&mut self, events: &[HostEvent], cx: &mut Context<Self>) {
        self.transition = None;

        let now = Instant::now();
        let mut failed = false;
        for event in events {
            if let Some(notice) = self.model.apply(event, now) {
                failed |= matches!(notice, Notice::Failure(_));
                cx.emit(NoticeRaised(notice));
            }
        }
        let restart = self.restart_when_stopped && !failed;
        self.restart_when_stopped = false;
        self.choose_display_on_start &= restart;
        self.model.stopped();

        if restart {
            self.start(cx);
            return;
        }
        if !failed {
            cx.emit(NoticeRaised(Notice::Info("Host stopped".to_owned())));
        }
        cx.notify();
    }

    fn choose_display(&mut self, cx: &mut Context<Self>) {
        if self.model.phase() != Phase::Running {
            return;
        }

        self.restart_when_stopped = true;
        self.choose_display_on_start = true;
        self.stop(cx);
        cx.emit(NoticeRaised(Notice::Info(
            "Restarting the host to choose a display".to_owned(),
        )));
        cx.notify();
    }

    fn renew_pin(&mut self, cx: &mut Context<Self>) {
        if self.model.pairing().is_none() {
            return;
        }

        if let Some(handle) = &self.handle {
            handle.renew_pin();
        }
        cx.notify();
    }

    fn disconnect(&mut self, cx: &mut Context<Self>) {
        if self.model.client().is_none() {
            return;
        }

        if let Some(handle) = &self.handle {
            handle.disconnect();
        }
        cx.notify();
    }

    fn revoke(&mut self, id: PeerId, name: &str, cx: &mut Context<Self>) {
        let Some(clients) = &self.clients else {
            return;
        };

        let notice = match clients.revoke(&id) {
            Ok(true) => Notice::Info(format!(
                "Revoked {name}. A client already streaming stays until it leaves"
            )),
            Ok(false) => Notice::Info(format!("{name} was already revoked")),
            Err(error) => {
                Notice::Failure(format!("Could not revoke {name}: {}", error_chain(&error)))
            }
        };
        cx.emit(NoticeRaised(notice));
        self.reload_paired();
        cx.notify();
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        let mut changed = self.reload_paired();

        if let Some(handle) = &self.handle {
            let mut ended = false;
            for event in handle.poll_events() {
                ended |= matches!(event, HostEvent::Stopped(_));
                if let Some(notice) = self.model.apply(&event, now) {
                    cx.emit(NoticeRaised(notice));
                }
                changed = true;
            }

            changed |= self.model.observe(handle.stream().as_ref(), now);
            changed |= self.model.pairing().is_some();

            if ended || handle.is_finished() {
                self.reap();
                changed = true;
            }
        }

        if changed {
            cx.notify();
        }
    }

    fn reap(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.stop();
        }
        self.model.stopped();
    }

    const fn tone(&self) -> Tone {
        match self.model.phase() {
            Phase::Stopped => Tone::Neutral,
            Phase::Starting | Phase::Stopping => Tone::Warning,
            Phase::Running if self.model.client().is_some() => Tone::Accent,
            Phase::Running => Tone::Success,
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (label, style, enabled) = match self.model.phase() {
            Phase::Stopped => ("Start hosting", ButtonStyle::Primary, true),
            Phase::Starting => ("Starting\u{2026}", ButtonStyle::Primary, false),
            Phase::Running => ("Stop hosting", ButtonStyle::Danger, true),
            Phase::Stopping => ("Stopping\u{2026}", ButtonStyle::Danger, false),
        };
        let running = self.model.phase() == Phase::Running;

        div()
            .flex()
            .items_end()
            .justify_between()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child("Host this PC"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_sm()
                            .text_color(theme::text_muted())
                            .child(status_dot(self.tone()))
                            .child(self.model.status()),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .pt_2()
                            .child(toggle("host-on-launch", self.launch_on_start).on_click(
                                cx.listener(|this, _, _, cx| {
                                    let enabled = !this.launch_on_start;
                                    this.set_launch_on_start(enabled, cx);
                                }),
                            ))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme::text())
                                    .child("Start hosting when the app opens"),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        button(
                            "choose-display",
                            "Choose display",
                            ButtonStyle::Secondary,
                            running,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.choose_display(cx))),
                    )
                    .child(
                        button("toggle-hosting", label, style, enabled).on_click(cx.listener(
                            move |this, _, _, cx| {
                                if running {
                                    this.stop(cx);
                                } else {
                                    this.start(cx);
                                }
                            },
                        )),
                    ),
            )
    }

    fn render_pairing(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let pairing = self.model.pairing();
        let body = match pairing {
            Some(pairing) => {
                let seconds = self.model.seconds_left(Instant::now()).unwrap_or(0);

                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(PIN_SIZE))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::accent_bright())
                                    .child(grouped_pin(pairing.pin.as_str())),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::text_muted())
                                    .child(format!("Valid for {seconds} s")),
                            ),
                    )
                    .child(
                        button("new-pin", "New PIN", ButtonStyle::Secondary, true)
                            .on_click(cx.listener(|this, _, _, cx| this.renew_pin(cx))),
                    )
                    .into_any_element()
            }
            None => div()
                .text_sm()
                .text_color(theme::text_muted())
                .child(match self.model.client() {
                    Some(_) => "A client is streaming, so no PIN is offered.",
                    None => "Start hosting to get a PIN a client can pair with.",
                })
                .into_any_element(),
        };

        card()
            .child(card_heading(
                "Pairing",
                Some("Enter this PIN on the client to pair it with this machine.".into()),
            ))
            .child(body)
    }

    fn render_client(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let heading = match self.model.client() {
            Some(client) => card_heading(
                if client.name.is_empty() {
                    "Connected client".to_owned()
                } else {
                    client.name.clone()
                },
                Some(client.address.to_string().into()),
            ),
            None => card_heading("Connected client", Some("Nobody is connected.".into())),
        };

        let mut rows: Vec<(&'static str, String)> = Vec::new();
        if let Some(format) = self.model.format_line() {
            rows.push(("Stream", format));
        }
        if let Some(rates) = self.model.rates() {
            rows.extend(rates.rows());
        }

        let badges = self.model.client().map(|client| {
            let (label, tone) = match client.admission {
                hosting::Admission::Paired => ("Just paired", Tone::Accent),
                hosting::Admission::Reconnected => ("Reconnected", Tone::Success),
            };

            div().flex().pb_3().child(badge(label, tone))
        });

        let disconnect = self.model.client().map(|_| {
            div().flex().pb_3().child(
                button("disconnect-client", "Disconnect", ButtonStyle::Danger, true)
                    .on_click(cx.listener(|this, _, _, cx| this.disconnect(cx))),
            )
        });

        card()
            .child(heading)
            .children(badges)
            .children(disconnect)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .children(rows.into_iter().map(|(label, value)| {
                        div()
                            .flex()
                            .items_center()
                            .gap_4()
                            .py_2()
                            .border_t_1()
                            .border_color(theme::border())
                            .child(
                                div()
                                    .w(px(ROW_LABEL_WIDTH))
                                    .flex_none()
                                    .text_sm()
                                    .text_color(theme::text_muted())
                                    .child(label),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .text_color(theme::text())
                                    .child(value),
                            )
                    })),
            )
    }

    fn render_paired(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let rows: Vec<_> = self
            .paired
            .iter()
            .enumerate()
            .map(|(index, client)| {
                let id = client.id;
                let name = client.name.clone();

                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .py_2()
                    .border_t_1()
                    .border_color(theme::border())
                    .child(
                        div()
                            .min_w_0()
                            .text_sm()
                            .text_color(theme::text())
                            .child(client.name.clone()),
                    )
                    .child(
                        button(
                            ("revoke-client", index),
                            "Revoke",
                            ButtonStyle::Danger,
                            true,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.revoke(id, &name, cx);
                        })),
                    )
            })
            .collect();

        card()
            .child(card_heading(
                "Paired clients",
                Some(client_summary(&self.paired).into()),
            ))
            .children(self.clients_error.clone().map(|error| {
                div()
                    .pb_3()
                    .text_xs()
                    .text_color(theme::warning())
                    .child(error)
            }))
            .child(div().flex().flex_col().children(rows))
    }
}

impl Render for HostPageView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let main = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap_5()
            .child(self.render_pairing(cx))
            .child(self.render_client(cx));

        div().id("host-page").size_full().overflow_y_scroll().child(
            div()
                .flex()
                .flex_col()
                .gap_5()
                .p_6()
                .child(self.render_header(cx))
                .child(
                    div().flex().items_start().gap_5().child(main).child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_none()
                            .w(px(SIDE_PANEL_WIDTH))
                            .child(self.render_paired(cx)),
                    ),
                ),
        )
    }
}
