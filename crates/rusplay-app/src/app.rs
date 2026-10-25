use gpui::{
    App, Context, Entity, FocusHandle, Focusable, KeyBinding, Subscription, Window, actions, div,
    prelude::*, px,
};
use rusplay_config::ConfigEnvironment;

use crate::{
    hosts::HostsView,
    notice::{Notice, NoticeRaised, error_chain},
    session, theme,
};

actions!(rusplay, [FocusNext, FocusPrevious, Quit]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", FocusNext, None),
        KeyBinding::new("shift-tab", FocusPrevious, None),
        KeyBinding::new("ctrl-q", Quit, None),
    ]);
}

pub struct RusplayApp {
    hosts: Entity<HostsView>,
    notice: Option<Notice>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl RusplayApp {
    pub fn new(environment: ConfigEnvironment, cx: &mut Context<Self>) -> Self {
        let (config, startup_notice) = match session::load_client_config(&environment) {
            Ok(config) => (config, None),
            Err(error) => (
                rusplay_config::Config::default(),
                Some(Notice::Failure(format!(
                    "Could not load the client config, using defaults: {}",
                    error_chain(&error)
                ))),
            ),
        };

        let hosts = cx.new(|cx| HostsView::new(config.network.port, environment, cx));
        let hosts_notices = cx.subscribe(&hosts, |this, _, event: &NoticeRaised, cx| {
            this.notice = Some(event.0.clone());
            cx.notify();
        });

        Self {
            hosts,
            notice: startup_notice,
            focus_handle: cx.focus_handle(),
            _subscriptions: vec![hosts_notices],
        }
    }

    fn render_status(&self) -> impl IntoElement {
        let (color, text) = match &self.notice {
            Some(Notice::Info(text)) => (theme::success(), text.as_str()),
            Some(Notice::Failure(text)) => (theme::danger(), text.as_str()),
            None => (theme::text_muted(), "Ready"),
        };

        div()
            .id("status")
            .px_4()
            .py_2()
            .min_h(px(36.0))
            .border_t_1()
            .border_color(theme::border())
            .bg(theme::surface())
            .text_sm()
            .text_color(color)
            .child(text.to_owned())
    }
}

impl Focusable for RusplayApp {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RusplayApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("root")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &FocusNext, window, cx| window.focus_next(cx)))
            .on_action(cx.listener(|_, _: &FocusPrevious, window, cx| window.focus_prev(cx)))
            .on_action(cx.listener(|_, _: &Quit, _, cx| cx.quit()))
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::background())
            .text_color(theme::text())
            .child(div().flex_1().child(self.hosts.clone()))
            .child(self.render_status())
    }
}
