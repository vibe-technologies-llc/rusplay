use gpui::{Context, Entity, EventEmitter, Subscription, Window, div, prelude::*, px};
use rusplay_config::ConfigEnvironment;

use crate::{
    hosts::model::{HostEntry, HostList},
    notice::{Notice, NoticeRaised, error_chain},
    session, theme,
    widgets::{
        controls::{ButtonStyle, button},
        text_field::{TextField, TextFieldEvent},
    },
};

pub struct HostsView {
    hosts: HostList,
    name_input: Entity<TextField>,
    address_input: Entity<TextField>,
    environment: ConfigEnvironment,
    connecting: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<NoticeRaised> for HostsView {}

impl HostsView {
    pub fn new(default_port: u16, environment: ConfigEnvironment, cx: &mut Context<Self>) -> Self {
        let name_input = cx.new(|cx| TextField::new("", "Name (optional)", cx));
        let address_input = cx.new(|cx| TextField::new("", "Address, e.g. 192.168.1.20", cx));

        let submit = cx.subscribe(&address_input, |this, _, event, cx| {
            if *event == TextFieldEvent::Submitted {
                this.add_host(cx);
            }
        });

        Self {
            hosts: HostList::new(default_port),
            name_input,
            address_input,
            environment,
            connecting: false,
            _subscriptions: vec![submit],
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
                notify(Notice::Info(format!("Added {address}")), cx);
            }
            Err(error) => notify(Notice::Failure(error.to_string()), cx),
        }

        cx.notify();
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        if self.connecting {
            return;
        }

        let Some(host) = self.hosts.selected().cloned() else {
            return;
        };

        self.connecting = true;
        notify(Notice::Info(format!("Connecting to {}", host.name)), cx);

        let environment = self.environment.clone();
        let session = cx.background_spawn(async move { session::run_client(&environment) });

        cx.spawn(async move |this, cx| {
            let result = session.await;

            this.update(cx, |this, cx| {
                this.connecting = false;
                let notice = match result {
                    Ok(()) => Notice::Info(format!("Session with {} ended", host.name)),
                    Err(error) => Notice::Failure(format!(
                        "Could not connect to {}: {}",
                        host.name,
                        error_chain(&error)
                    )),
                };
                notify(notice, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();

        cx.notify();
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
                            .child(host.address.to_string()),
                    ),
            )
            .child(
                button(("remove-host", index), "Remove", ButtonStyle::Quiet, true).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.hosts.remove(index);
                        cx.stop_propagation();
                        cx.notify();
                    }),
                ),
            )
    }
}

impl Render for HostsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entries: Vec<_> = self.hosts.entries().to_vec();
        let can_connect = self.hosts.selected().is_some() && !self.connecting;
        let connect_label = if self.connecting {
            "Connecting"
        } else {
            "Connect"
        };

        let list = if entries.is_empty() {
            div()
                .py_6()
                .text_color(theme::text_muted())
                .child("No hosts yet. Add one by address below; discovery will list hosts here.")
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
            .child(div().text_xl().text_color(theme::text()).child("Hosts"))
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
            .child(
                div().flex().child(
                    button("connect", connect_label, ButtonStyle::Primary, can_connect)
                        .on_click(cx.listener(|this, _, _, cx| this.connect(cx))),
                ),
            )
    }
}

fn notify(notice: Notice, cx: &mut Context<HostsView>) {
    cx.emit(NoticeRaised(notice));
}
