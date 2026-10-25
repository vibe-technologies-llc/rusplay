use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, KeyBinding, Subscription, Window,
    actions, div, prelude::*, px,
};
use rusplay_config::{Config, ConfigEnvironment};

use crate::{
    hosts::HostsView,
    notice::{Notice, NoticeRaised, error_chain},
    session,
    settings::{FormModel, SettingsView},
    theme,
    widgets::controls::option_button,
};

actions!(rusplay, [FocusNext, FocusPrevious, Quit]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", FocusNext, None),
        KeyBinding::new("shift-tab", FocusPrevious, None),
        KeyBinding::new("ctrl-q", Quit, None),
    ]);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Hosts,
    Settings,
}

impl Page {
    const ALL: [Self; 2] = [Self::Hosts, Self::Settings];

    const fn title(self) -> &'static str {
        match self {
            Self::Hosts => "Hosts",
            Self::Settings => "Settings",
        }
    }
}

pub struct RusplayApp {
    page: Page,
    hosts: Entity<HostsView>,
    settings: Option<Entity<SettingsView>>,
    notice: Option<Notice>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl RusplayApp {
    pub fn new(environment: ConfigEnvironment, cx: &mut Context<Self>) -> Self {
        let mut notice = None;

        let config = session::load_client_config(&environment).unwrap_or_else(|error| {
            notice = Some(Notice::Failure(format!(
                "Could not load the client config, using defaults: {}",
                error_chain(&error)
            )));
            Config::default()
        });

        let mut subscriptions = Vec::new();

        let hosts = cx.new(|cx| HostsView::new(config.network.port, environment.clone(), cx));
        subscriptions.push(cx.subscribe(&hosts, |this, _, event: &NoticeRaised, cx| {
            this.notice = Some(event.0.clone());
            cx.notify();
        }));

        let settings = match FormModel::from_config(&config) {
            Ok(form) => {
                let settings = cx.new(|cx| SettingsView::new(config, form, environment, cx));
                subscriptions.push(
                    cx.subscribe(&settings, |this, _, event: &NoticeRaised, cx| {
                        this.notice = Some(event.0.clone());
                        cx.notify();
                    }),
                );
                Some(settings)
            }
            Err(error) => {
                notice = Some(Notice::Failure(format!(
                    "Settings are unavailable: {}",
                    error_chain(&error)
                )));
                None
            }
        };

        Self {
            page: Page::Hosts,
            hosts,
            settings,
            notice,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    fn render_navigation(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let mut tabs = Vec::with_capacity(Page::ALL.len());

        for (position, page) in Page::ALL.into_iter().enumerate() {
            tabs.push(
                option_button(("page", position), page.title(), self.page == page).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.page = page;
                        cx.notify();
                    }),
                ),
            );
        }

        div()
            .flex()
            .items_center()
            .gap_2()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(theme::border())
            .bg(theme::surface())
            .child(div().pr_4().text_color(theme::text()).child("Rusplay"))
            .children(tabs)
    }

    fn render_page(&self) -> AnyElement {
        match (self.page, &self.settings) {
            (Page::Hosts, _) => self.hosts.clone().into_any_element(),
            (Page::Settings, Some(settings)) => settings.clone().into_any_element(),
            (Page::Settings, None) => div()
                .p_6()
                .text_color(theme::danger())
                .child("Settings could not be built from the current configuration.")
                .into_any_element(),
        }
    }

    fn render_status(&self) -> impl IntoElement + use<> {
        let (color, text) = match &self.notice {
            Some(Notice::Info(text)) => (theme::success(), text.clone()),
            Some(Notice::Failure(text)) => (theme::danger(), text.clone()),
            None => (theme::text_muted(), "Ready".to_owned()),
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
            .child(text)
    }
}

impl Focusable for RusplayApp {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RusplayApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let navigation = self.render_navigation(cx);

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
            .child(navigation)
            .child(div().flex_1().min_h_0().child(self.render_page()))
            .child(self.render_status())
    }
}
