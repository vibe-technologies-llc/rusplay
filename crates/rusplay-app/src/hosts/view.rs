use gpui::{
    Context, Entity, EventEmitter, FontWeight, Subscription, Task, Window, div, prelude::*, px,
};
use rusplay_client::{Credentials, DiscoveredHost, HostBook, PeerRecord, Pin};
use rusplay_config::{Config, ConfigEnvironment, HostProfile};

use crate::{
    hosts::{
        model::{HostEntry, HostId, HostList, HostOrigin},
        profile::{self, ProfileRow, Source},
    },
    notice::{Notice, NoticeRaised, error_chain},
    session::{self, SessionError},
    theme,
    widgets::{
        controls::{ButtonStyle, Tone, badge, button, card, card_heading, option_button},
        text_field::{TextField, TextFieldEvent},
    },
};

const SIDE_PANEL_WIDTH: f32 = 330.0;
const AVATAR_SIZE: f32 = 36.0;
const AVAILABILITY_DOT: f32 = 6.0;
const PROFILE_LABEL_WIDTH: f32 = 130.0;

#[derive(Debug, Clone)]
pub struct ConnectRequested {
    pub host: HostEntry,
    pub credentials: Credentials,
}

pub struct HostsView {
    hosts: HostList,
    book: Option<HostBook>,
    environment: ConfigEnvironment,
    global: Config,
    scan: Option<Task<()>>,
    name_input: Entity<TextField>,
    address_input: Entity<TextField>,
    pin_input: Entity<TextField>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<NoticeRaised> for HostsView {}
impl EventEmitter<ConnectRequested> for HostsView {}

impl HostsView {
    pub fn new(
        default_port: u16,
        environment: ConfigEnvironment,
        global: Config,
        book: Option<HostBook>,
        cx: &mut Context<Self>,
    ) -> Self {
        let name_input = cx.new(|cx| TextField::new("", "Name (optional)", cx));
        let address_input = cx.new(|cx| TextField::new("", "Address, e.g. 192.168.1.20", cx));
        let pin_input = cx.new(|cx| TextField::new("", "PIN shown by the host", cx));

        let submit = cx.subscribe(&address_input, |this, _, event, cx| {
            if *event == TextFieldEvent::Submitted {
                this.add_host(cx);
            }
        });

        let connect = cx.subscribe(&pin_input, |this, _, event, cx| {
            if *event == TextFieldEvent::Submitted {
                this.connect(cx);
            }
        });

        Self {
            hosts: book.as_ref().map_or_else(
                || HostList::new(default_port),
                |book| HostList::from_saved(default_port, book.hosts()),
            ),
            book,
            environment,
            global,
            scan: None,
            name_input,
            address_input,
            pin_input,
            _subscriptions: vec![submit, connect],
        }
    }

    pub fn remember_peer(&mut self, id: HostId, peer: PeerRecord, cx: &mut Context<Self>) -> bool {
        let remembered = self.hosts.remember_peer(id, peer);
        if remembered {
            self.persist(cx);
            self.pin_input
                .update(cx, |input, cx| input.set_text("", cx));
            cx.notify();
        }

        remembered
    }

    pub fn request_connection(&mut self, target: &str, cx: &mut Context<Self>) {
        match self.hosts.find(target) {
            Ok(index) => {
                self.hosts.select(index);
                self.connect(cx);
                cx.notify();
            }
            Err(error) => notify(Notice::Failure(error.to_string()), cx),
        }
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.reload_global(cx);

        if self.scan.is_some() {
            return;
        }

        let environment = self.environment.clone();
        let scanning = cx.background_spawn(async move { session::discover(&environment) });
        self.scan = Some(cx.spawn(async move |this, cx| {
            let result = scanning.await;
            this.update(cx, |this, cx| this.scanned(result, cx)).ok();
        }));
        cx.notify();
    }

    fn reload_global(&mut self, cx: &mut Context<Self>) {
        match session::load_client_config(&self.environment) {
            Ok(global) => self.global = global,
            Err(error) => notify(
                Notice::Failure(format!(
                    "Could not load the client config for the host profiles: {}",
                    error_chain(&error)
                )),
                cx,
            ),
        }
    }

    fn set_selected_profile(&mut self, profile: HostProfile, cx: &mut Context<Self>) {
        let Some(index) = self.hosts.selected_index() else {
            return;
        };

        if let Err(error) = profile.apply(&self.global) {
            notify(
                Notice::Failure(format!(
                    "That override does not work with the client config: {}",
                    error_chain(&error)
                )),
                cx,
            );
            return;
        }

        self.hosts.set_profile(index, profile);
        self.persist(cx);
        cx.notify();
    }

    fn scanned(
        &mut self,
        result: Result<Vec<DiscoveredHost>, SessionError>,
        cx: &mut Context<Self>,
    ) {
        self.scan = None;

        match result {
            Ok(found) => {
                let before = self.hosts.saved_hosts();
                self.hosts.merge_discovered(&found);
                if self.hosts.saved_hosts() != before {
                    self.persist(cx);
                }
            }
            Err(error) => notify(
                Notice::Failure(format!("Could not scan for hosts: {}", error_chain(&error))),
                cx,
            ),
        }

        cx.notify();
    }

    fn persist(&mut self, cx: &mut Context<Self>) {
        let Some(book) = &mut self.book else {
            return;
        };

        if let Err(error) = book.save(self.hosts.saved_hosts()) {
            notify(
                Notice::Failure(format!(
                    "Could not save the host book: {}",
                    error_chain(&error)
                )),
                cx,
            );
        }
    }

    fn rename_selected(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.hosts.selected_index() else {
            return;
        };
        let name = self.name_input.read(cx).text().to_owned();

        let result = self.hosts.rename(index, &name);
        self.name_input
            .update(cx, |input, cx| input.set_invalid(result.is_err(), cx));

        match result {
            Ok(()) => {
                self.name_input
                    .update(cx, |input, cx| input.set_text("", cx));
                self.persist(cx);
            }
            Err(error) => notify(Notice::Failure(error.to_string()), cx),
        }

        cx.notify();
    }

    fn change_selected_address(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.hosts.selected_index() else {
            return;
        };
        let address = self.address_input.read(cx).text().to_owned();

        let result = self.hosts.set_address(index, &address);
        self.address_input
            .update(cx, |input, cx| input.set_invalid(result.is_err(), cx));

        match result {
            Ok(()) => {
                self.address_input
                    .update(cx, |input, cx| input.set_text("", cx));
                self.persist(cx);
            }
            Err(error) => notify(Notice::Failure(error.to_string()), cx),
        }

        cx.notify();
    }

    fn forget_selected_pairing(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.hosts.selected_index() else {
            return;
        };

        if self.hosts.forget_pairing(index) {
            self.persist(cx);
            notify(
                Notice::Info("Forgot the pairing; the host needs a new PIN".to_owned()),
                cx,
            );
            cx.notify();
        }
    }

    fn add_host(&mut self, cx: &mut Context<Self>) {
        let name = self.name_input.read(cx).text().to_owned();
        let address = self.address_input.read(cx).text().to_owned();

        let result = self.hosts.add(&name, &address);
        self.address_input
            .update(cx, |input, cx| input.set_invalid(result.is_err(), cx));

        match result {
            Ok(_) => {
                self.name_input
                    .update(cx, |input, cx| input.set_text("", cx));
                self.address_input
                    .update(cx, |input, cx| input.set_text("", cx));
                self.persist(cx);
                notify(Notice::Info(format!("Added {address}")), cx);
            }
            Err(error) => notify(Notice::Failure(error.to_string()), cx),
        }

        cx.notify();
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        let Some(host) = self.hosts.selected().cloned() else {
            return;
        };

        let credentials = if let Some(peer) = &host.peer {
            Credentials::Peer(peer.clone())
        } else {
            let parsed = Pin::parse(self.pin_input.read(cx).text());
            self.pin_input
                .update(cx, |input, cx| input.set_invalid(parsed.is_err(), cx));
            match parsed {
                Ok(pin) => Credentials::Pin(pin),
                Err(error) => {
                    notify(
                        Notice::Failure(format!("Pair with {} first: {error}", host.name)),
                        cx,
                    );
                    return;
                }
            }
        };

        cx.emit(ConnectRequested { host, credentials });
    }

    fn render_host(
        &self,
        index: usize,
        host: &HostEntry,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let selected = self.hosts.selected_index() == Some(index);
        let origin = match host.origin {
            HostOrigin::Added => badge("Saved", Tone::Neutral),
            HostOrigin::Discovered => badge("Discovered", Tone::Accent),
        };

        div()
            .id(("host", index))
            .flex()
            .items_center()
            .gap_3()
            .p_3()
            .rounded_lg()
            .border_1()
            .cursor_pointer()
            .map(|this| {
                if selected {
                    this.border_color(theme::accent())
                        .bg(theme::surface_raised())
                        .shadow(theme::glow())
                } else {
                    this.border_color(theme::border())
                        .bg(theme::surface())
                        .hover(|style| {
                            style
                                .bg(theme::surface_raised())
                                .border_color(theme::border_strong())
                        })
                }
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.hosts.select(index);
                cx.notify();
            }))
            .child(avatar(&host.name, selected))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme::text())
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(host.name.clone()),
                            )
                            .children(host.peer.is_some().then(|| badge("Paired", Tone::Success)))
                            .child(origin),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child(
                                div()
                                    .size(px(AVAILABILITY_DOT))
                                    .rounded_full()
                                    .bg(availability_color(host)),
                            )
                            .child(format!("{} \u{00b7} {}", host.address, availability(host))),
                    ),
            )
            .child(
                button(("remove-host", index), "Remove", ButtonStyle::Danger, true).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.hosts.remove(index);
                        this.persist(cx);
                        cx.stop_propagation();
                        cx.notify();
                    }),
                ),
            )
    }

    fn render_profile(&self, host: &HostEntry, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let rows: Vec<_> = profile::rows(&host.profile, &self.global)
            .into_iter()
            .enumerate()
            .map(|(position, row)| Self::render_profile_row(position, row, cx))
            .collect();
        let overridden = !host.profile.is_empty();

        card()
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .gap_4()
                    .child(card_heading(
                        format!("Client settings for {}", host.name),
                        Some(
                            "Global follows client.toml; a choice overrides it for this host only."
                                .into(),
                        ),
                    ))
                    .child(
                        button(
                            "clear-profile",
                            "Reset all to global",
                            ButtonStyle::Quiet,
                            overridden,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.set_selected_profile(HostProfile::default(), cx);
                        })),
                    ),
            )
            .child(div().flex().flex_col().children(rows))
    }

    fn render_profile_row(
        position: usize,
        row: ProfileRow,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let source_tone = match row.source {
            Source::Override => Tone::Accent,
            Source::Global => Tone::Neutral,
        };
        let choices: Vec<_> = row
            .choices
            .into_iter()
            .enumerate()
            .map(|(choice_position, choice)| {
                let result = choice.result;
                option_button(
                    ("profile-choice", position * 16 + choice_position),
                    choice.label,
                    choice.selected,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.set_selected_profile(result.clone(), cx);
                }))
            })
            .collect();

        div()
            .flex()
            .items_center()
            .gap_4()
            .py_2p5()
            .border_t_1()
            .border_color(theme::border())
            .child(
                div()
                    .w(px(PROFILE_LABEL_WIDTH))
                    .flex_none()
                    .text_sm()
                    .text_color(theme::text())
                    .child(row.label),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .children(choices),
            )
            .child(badge(
                format!("{} \u{00b7} {}", row.effective, row.source.label()),
                source_tone,
            ))
    }

    fn render_list(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let entries: Vec<_> = self.hosts.entries().to_vec();

        let body = if entries.is_empty() {
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_2()
                .py_10()
                .rounded_lg()
                .border_1()
                .border_color(theme::border())
                .bg(theme::inset())
                .child(
                    div()
                        .text_color(theme::text())
                        .font_weight(FontWeight::MEDIUM)
                        .child("No hosts yet"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme::text_muted())
                        .child("Refresh to scan the network again, or add one by address."),
                )
                .into_any_element()
        } else {
            let mut rows = Vec::with_capacity(entries.len());
            for (index, host) in entries.iter().enumerate() {
                rows.push(self.render_host(index, host, cx));
            }

            div()
                .flex()
                .flex_col()
                .gap_2()
                .children(rows)
                .into_any_element()
        };

        card()
            .child(card_heading(
                "Your hosts",
                Some("Saved hosts and the ones announcing themselves on the network.".into()),
            ))
            .child(body)
    }

    fn render_connect(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let selected = self.hosts.selected();
        let can_connect = selected.is_some();
        let paired = selected.is_some_and(|host| host.peer.is_some());

        let heading = match selected {
            Some(host) => card_heading(host.name.clone(), Some(host.address.to_string().into())),
            None => card_heading("Connect", Some("Select a host from the list.".into())),
        };

        let credentials = if paired {
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(badge("Paired", Tone::Success))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child("Connects with the remembered pairing"),
                )
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(field_label("PIN"))
                .child(self.pin_input.clone())
                .into_any_element()
        };

        card()
            .child(heading)
            .child(credentials)
            .child(
                div().pt_4().child(
                    button("connect", "Connect", ButtonStyle::Primary, can_connect)
                        .w_full()
                        .on_click(cx.listener(|this, _, _, cx| this.connect(cx))),
                ),
            )
            .children(paired.then(|| {
                div().pt_2().child(
                    button(
                        "forget-pairing",
                        "Forget pairing",
                        ButtonStyle::Danger,
                        true,
                    )
                    .w_full()
                    .on_click(cx.listener(|this, _, _, cx| this.forget_selected_pairing(cx))),
                )
            }))
    }

    fn render_editor(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let can_edit = self.hosts.selected().is_some();

        card()
            .child(card_heading(
                "Add or edit a host",
                Some("Add a new host, or apply the name or address to the selected one.".into()),
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1p5()
                            .child(field_label("Name"))
                            .child(self.name_input.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1p5()
                            .child(field_label("Address"))
                            .child(self.address_input.clone()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .pt_4()
                    .child(
                        button("add-host", "Add", ButtonStyle::Secondary, true)
                            .on_click(cx.listener(|this, _, _, cx| this.add_host(cx))),
                    )
                    .child(
                        button("rename-host", "Rename", ButtonStyle::Quiet, can_edit)
                            .on_click(cx.listener(|this, _, _, cx| this.rename_selected(cx))),
                    )
                    .child(
                        button(
                            "change-address",
                            "Change address",
                            ButtonStyle::Quiet,
                            can_edit,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.change_selected_address(cx))),
                    ),
            )
    }
}

impl Render for HostsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let profile_panel = self
            .hosts
            .selected()
            .cloned()
            .map(|host| self.render_profile(&host, cx));
        let scanning = self.scan.is_some();
        let count = self.hosts.entries().len();
        let summary = match (count, scanning) {
            (_, true) => "Scanning the network\u{2026}".to_owned(),
            (0, false) => "No hosts".to_owned(),
            (1, false) => "1 host".to_owned(),
            (count, false) => format!("{count} hosts"),
        };

        div()
            .id("hosts-page")
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .p_6()
                    .child(
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
                                            .child("Hosts"),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(theme::text_muted())
                                            .child(summary),
                                    ),
                            )
                            .child(
                                button(
                                    "refresh-hosts",
                                    if scanning {
                                        "Scanning\u{2026}"
                                    } else {
                                        "Refresh"
                                    },
                                    ButtonStyle::Secondary,
                                    !scanning,
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .gap_5()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_5()
                                    .child(self.render_list(cx))
                                    .children(profile_panel),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_none()
                                    .w(px(SIDE_PANEL_WIDTH))
                                    .gap_5()
                                    .child(self.render_connect(cx))
                                    .child(self.render_editor(cx)),
                            ),
                    ),
            )
    }
}

fn avatar(name: &str, selected: bool) -> impl IntoElement + use<> {
    let initial = name
        .chars()
        .find(|character| character.is_alphanumeric())
        .map_or_else(
            || "?".to_owned(),
            |character| character.to_uppercase().collect(),
        );

    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(AVATAR_SIZE))
        .rounded_lg()
        .font_weight(FontWeight::SEMIBOLD)
        .map(|this| {
            if selected {
                this.bg(theme::brand()).text_color(theme::accent_text())
            } else {
                this.bg(theme::surface_hover())
                    .text_color(theme::text_muted())
            }
        })
        .child(initial)
}

fn field_label(label: &'static str) -> impl IntoElement + use<> {
    div()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::text_muted())
        .child(label)
}

fn availability_color(host: &HostEntry) -> gpui::Rgba {
    if host.online {
        theme::success()
    } else {
        theme::text_subtle()
    }
}

const fn availability(host: &HostEntry) -> &'static str {
    match (host.online, host.origin) {
        (true, HostOrigin::Discovered) => "found on the network",
        (true, HostOrigin::Added) => "online",
        (false, _) => "not seen",
    }
}

fn notify(notice: Notice, cx: &mut Context<HostsView>) {
    cx.emit(NoticeRaised(notice));
}
