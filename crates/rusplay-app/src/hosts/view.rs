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
        controls::{
            ButtonStyle, Tone, button, option_button, panel, panel_heading, selection_bar,
            state_text,
        },
        text_field::{TextField, TextFieldEvent},
    },
};

const SIDE_PANEL_WIDTH: f32 = 320.0;
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

    fn remove_selected(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.hosts.selected_index() else {
            return;
        };

        self.hosts.remove(index);
        self.persist(cx);
        cx.notify();
    }

    fn render_host(
        &self,
        index: usize,
        host: &HostEntry,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let selected = self.hosts.selected_index() == Some(index);
        let (state, tone) = host_state(host);

        div()
            .id(("host", index))
            .relative()
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .py_2p5()
            .cursor_pointer()
            .when(index > 0, |this| {
                this.border_t_1().border_color(theme::rule())
            })
            .map(|this| {
                if selected {
                    this.bg(theme::raised()).child(selection_bar())
                } else {
                    this.hover(|style| style.bg(theme::raised()))
                }
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.hosts.select(index);
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .text_color(theme::text())
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(host.name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_features(theme::tabular())
                            .text_color(theme::text_faint())
                            .child(host.address.to_string()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .when(host.origin == HostOrigin::Discovered, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme::text_faint())
                                .child("on network"),
                        )
                    })
                    .child(state_text(state, tone)),
            )
    }

    fn render_profile(&self, host: &HostEntry, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let rows: Vec<_> = profile::rows(&host.profile, &self.global)
            .into_iter()
            .enumerate()
            .map(|(position, row)| Self::render_profile_row(position, row, cx))
            .collect();
        let overridden = !host.profile.is_empty();

        panel()
            .child(panel_heading(
                format!("Overrides for {}", host.name),
                Some(
                    button(
                        "clear-profile",
                        "Reset all to global",
                        ButtonStyle::Quiet,
                        overridden,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_selected_profile(HostProfile::default(), cx);
                    }))
                    .into_any_element(),
                ),
            ))
            .child(
                div()
                    .pb_2()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child("Global follows client.toml; a choice overrides it for this host only."),
            )
            .child(div().flex().flex_col().children(rows))
    }

    fn render_profile_row(
        position: usize,
        row: ProfileRow,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let source_color = match row.source {
            Source::Override => theme::accent(),
            Source::Global => theme::text_faint(),
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
            .py_2()
            .border_t_1()
            .border_color(theme::rule())
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
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_baseline()
                    .gap_1p5()
                    .text_xs()
                    .child(
                        div()
                            .font_features(theme::tabular())
                            .text_color(theme::text())
                            .child(row.effective),
                    )
                    .child(div().text_color(source_color).child(row.source.label())),
            )
    }

    fn render_list(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let entries: Vec<_> = self.hosts.entries().to_vec();

        if entries.is_empty() {
            return panel()
                .text_sm()
                .text_color(theme::text_muted())
                .child("No hosts found. Add one by address, or Refresh to scan again.");
        }

        let mut rows = Vec::with_capacity(entries.len());
        for (index, host) in entries.iter().enumerate() {
            rows.push(self.render_host(index, host, cx));
        }

        panel().p_0().overflow_hidden().children(rows)
    }

    fn render_detail(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let Some(host) = self.hosts.selected() else {
            return panel().child(
                div()
                    .text_sm()
                    .text_color(theme::text_muted())
                    .child("Select a host to connect."),
            );
        };
        let paired = host.peer.is_some();

        let credentials = if paired {
            div()
                .text_sm()
                .text_color(theme::text_muted())
                .child("Paired. Connects without a PIN.")
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

        panel()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .pb_4()
                    .child(
                        div()
                            .truncate()
                            .text_color(theme::text())
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(host.name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_features(theme::tabular())
                            .text_color(theme::text_faint())
                            .child(host.address.to_string()),
                    )
                    .when(host.busy, |this| {
                        this.child(
                            div()
                                .pt_1()
                                .text_xs()
                                .text_color(theme::warning())
                                .child("Busy in another session"),
                        )
                    }),
            )
            .child(credentials)
            .child(
                div().pt_4().child(
                    button("connect", "Connect", ButtonStyle::Primary, true)
                        .w_full()
                        .on_click(cx.listener(|this, _, _, cx| this.connect(cx))),
                ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .pt_2()
                    .when(paired, |this| {
                        this.child(
                            button("forget-pairing", "Forget pairing", ButtonStyle::Quiet, true)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.forget_selected_pairing(cx)),
                                ),
                        )
                    })
                    .child(
                        button("remove-host", "Remove", ButtonStyle::Danger, true)
                            .on_click(cx.listener(|this, _, _, cx| this.remove_selected(cx))),
                    ),
            )
    }

    fn render_editor(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let can_edit = self.hosts.selected().is_some();

        panel()
            .child(panel_heading("Add or edit", None))
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
                    .gap_1()
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
            (_, true) => "scanning\u{2026}".to_owned(),
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
                    .gap_4()
                    .p_6()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .items_baseline()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_lg()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme::text())
                                            .child("Hosts"),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_features(theme::tabular())
                                            .text_color(theme::text_faint())
                                            .child(summary),
                                    ),
                            )
                            .child(
                                button(
                                    "refresh-hosts",
                                    "Refresh",
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
                            .gap_4()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_4()
                                    .child(self.render_list(cx))
                                    .children(profile_panel),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_none()
                                    .w(px(SIDE_PANEL_WIDTH))
                                    .gap_4()
                                    .child(self.render_detail(cx))
                                    .child(self.render_editor(cx)),
                            ),
                    ),
            )
    }
}

fn field_label(label: &'static str) -> impl IntoElement + use<> {
    div()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::text_muted())
        .child(label)
}

const fn host_state(host: &HostEntry) -> (&'static str, Tone) {
    match (host.busy, host.peer.is_some(), host.online) {
        (true, ..) => ("Busy", Tone::Warning),
        (false, true, true) => ("Paired", Tone::Live),
        (false, _, false) => ("Not seen", Tone::Neutral),
        (false, false, true) => ("Not paired", Tone::Neutral),
    }
}

fn notify(notice: Notice, cx: &mut Context<HostsView>) {
    cx.emit(NoticeRaised(notice));
}
