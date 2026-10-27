use std::net::SocketAddr;

use gpui::{Context, Entity, EventEmitter, Subscription, Task, Window, div, prelude::*, px};
use rusplay_client::{Credentials, DiscoveredHost, HostBook, PeerRecord, Pin};
use rusplay_config::ConfigEnvironment;

use crate::{
    hosts::model::{HostEntry, HostList, HostOrigin},
    notice::{Notice, NoticeRaised, error_chain},
    session::{self, SessionError},
    theme,
    widgets::{
        controls::{ButtonStyle, button},
        text_field::{TextField, TextFieldEvent},
    },
};

#[derive(Debug, Clone)]
pub struct ConnectRequested {
    pub host: HostEntry,
    pub credentials: Credentials,
}

pub struct HostsView {
    hosts: HostList,
    book: Option<HostBook>,
    environment: ConfigEnvironment,
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
            scan: None,
            name_input,
            address_input,
            pin_input,
            _subscriptions: vec![submit, connect],
        }
    }

    pub fn remember_peer(&mut self, address: SocketAddr, peer: PeerRecord, cx: &mut Context<Self>) {
        if self.hosts.remember_peer(address, peer) {
            self.persist(cx);
            self.pin_input
                .update(cx, |input, cx| input.set_text("", cx));
            cx.notify();
        }
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
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
        let background = if selected {
            theme::surface_raised()
        } else {
            theme::surface()
        };
        let border = if selected {
            theme::accent()
        } else {
            theme::border()
        };

        div()
            .id(("host", index))
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(border)
            .bg(background)
            .cursor_pointer()
            .hover(|style| style.bg(theme::surface_hover()))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.hosts.select(index);
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(div().text_color(theme::text()).child(host.name.clone()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child(format!("{} - {}", host.address, availability(host))),
                    ),
            )
            .child(
                button(("remove-host", index), "Remove", ButtonStyle::Quiet, true).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.hosts.remove(index);
                        this.persist(cx);
                        cx.stop_propagation();
                        cx.notify();
                    }),
                ),
            )
    }

    fn render_selected_actions(
        can_connect: bool,
        paired: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .text_color(theme::text_muted())
                    .child("Selected host, using the fields above:"),
            )
            .child(
                button("rename-host", "Rename", ButtonStyle::Secondary, can_connect)
                    .on_click(cx.listener(|this, _, _, cx| this.rename_selected(cx))),
            )
            .child(
                button(
                    "change-address",
                    "Change address",
                    ButtonStyle::Secondary,
                    can_connect,
                )
                .on_click(cx.listener(|this, _, _, cx| this.change_selected_address(cx))),
            )
            .child(
                button(
                    "forget-pairing",
                    "Forget pairing",
                    ButtonStyle::Quiet,
                    paired,
                )
                .on_click(cx.listener(|this, _, _, cx| this.forget_selected_pairing(cx))),
            )
    }
}

impl Render for HostsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entries: Vec<_> = self.hosts.entries().to_vec();
        let selected = self.hosts.selected();
        let can_connect = selected.is_some();
        let paired = selected.is_some_and(|host| host.peer.is_some());
        let scanning = self.scan.is_some();

        let list = if entries.is_empty() {
            div()
                .py_6()
                .text_color(theme::text_muted())
                .child("No hosts found. Refresh to scan the network again, or add one by address below.")
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

        div()
            .flex()
            .flex_col()
            .gap_4()
            .p_6()
            .size_full()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_xl().text_color(theme::text()).child("Hosts"))
                    .child(
                        button(
                            "refresh-hosts",
                            if scanning { "Scanning..." } else { "Refresh" },
                            ButtonStyle::Secondary,
                            !scanning,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    ),
            )
            .child(list)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().w(px(200.0)).child(self.name_input.clone()))
                    .child(div().w(px(280.0)).child(self.address_input.clone()))
                    .child(
                        button("add-host", "Add", ButtonStyle::Secondary, true)
                            .on_click(cx.listener(|this, _, _, cx| this.add_host(cx))),
                    ),
            )
            .child(Self::render_selected_actions(can_connect, paired, cx))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(if paired {
                        div()
                            .w(px(200.0))
                            .text_sm()
                            .text_color(theme::text_muted())
                            .child("Paired")
                            .into_any_element()
                    } else {
                        div()
                            .w(px(200.0))
                            .child(self.pin_input.clone())
                            .into_any_element()
                    })
                    .child(
                        button("connect", "Connect", ButtonStyle::Primary, can_connect)
                            .on_click(cx.listener(|this, _, _, cx| this.connect(cx))),
                    ),
            )
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
