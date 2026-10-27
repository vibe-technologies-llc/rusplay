use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, KeyBinding, Subscription, Window,
    actions, div, prelude::*, px,
};
use rusplay_client::HostBook;
use rusplay_config::{Config, ConfigEnvironment};

use crate::{
    hosts::{ConnectRequested, HostsView},
    notice::{Notice, NoticeRaised, error_chain},
    session,
    settings::{FormModel, SettingsView},
    stream::{StreamEvent, StreamView},
    theme,
    widgets::controls::option_button,
};

actions!(rusplay, [FocusNext, FocusPrevious, Quit, ToggleFullscreen]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", FocusNext, None),
        KeyBinding::new("shift-tab", FocusPrevious, None),
        KeyBinding::new("ctrl-q", Quit, None),
        KeyBinding::new("f11", ToggleFullscreen, None),
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

struct ActiveStream {
    view: Entity<StreamView>,
    _events: Subscription,
}

pub struct RusplayApp {
    page: Page,
    environment: ConfigEnvironment,
    stream: Option<ActiveStream>,
    hosts: Entity<HostsView>,
    settings: Option<Entity<SettingsView>>,
    notice: Option<Notice>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl RusplayApp {
    pub fn new(
        environment: ConfigEnvironment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut notice = None;

        let config = session::load_client_config(&environment).unwrap_or_else(|error| {
            notice = Some(Notice::Failure(format!(
                "Could not load the client config, using defaults: {}",
                error_chain(&error)
            )));
            Config::default()
        });

        let mut subscriptions = Vec::new();

        let book = HostBook::open(&config.peers)
            .inspect_err(|error| {
                notice = Some(Notice::Failure(format!(
                    "Hosts will not be saved: {}",
                    error_chain(error)
                )));
            })
            .ok();
        let hosts = cx.new(|cx| {
            HostsView::new(
                config.network.port,
                environment.clone(),
                config.clone(),
                book,
                cx,
            )
        });
        hosts.update(cx, HostsView::refresh);
        subscriptions.push(cx.subscribe(&hosts, |this, _, event: &NoticeRaised, cx| {
            this.notice = Some(event.0.clone());
            cx.notify();
        }));
        subscriptions.push(cx.subscribe_in(
            &hosts,
            window,
            |this, _, event: &ConnectRequested, window, cx| {
                this.start_stream(event.clone(), window, cx);
            },
        ));

        let app = cx.weak_entity();
        window.on_window_should_close(cx, move |_, cx| {
            app.update(cx, Self::end_stream).ok();
            true
        });

        let settings = match FormModel::from_config(&config) {
            Ok(form) => {
                let settings =
                    cx.new(|cx| SettingsView::new(config, form, environment.clone(), cx));
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
            environment,
            stream: None,
            hosts,
            settings,
            notice,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    fn start_stream(
        &mut self,
        request: ConnectRequested,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.stream.is_some() {
            return;
        }

        let attached = session::load_client_config(&self.environment)
            .map_err(session::SessionError::from)
            .and_then(|config| session::merge_profile(&request.host.profile, &config))
            .and_then(|config| {
                session::attach_window(window, &config).map(|attached| (config, attached))
            });
        let (config, attached) = match attached {
            Ok(attached) => attached,
            Err(error) => {
                self.notice = Some(Notice::Failure(format!(
                    "Could not open the stream view: {}",
                    error_chain(&error)
                )));
                cx.notify();
                return;
            }
        };

        let host_name = request.host.name.clone();
        let stream = cx.new(|cx| {
            StreamView::new(
                request.host,
                request.credentials,
                config,
                attached,
                window,
                cx,
            )
        });
        let events = cx.subscribe_in(
            &stream,
            window,
            |this, _, event: &StreamEvent, window, cx| this.stream_event(event, window, cx),
        );
        window.focus(&stream.focus_handle(cx), cx);

        self.stream = Some(ActiveStream {
            view: stream,
            _events: events,
        });
        self.notice = Some(Notice::Info(format!("Connecting to {host_name}")));
        cx.notify();
    }

    fn stream_event(&mut self, event: &StreamEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            StreamEvent::Paired { address, peer } => {
                let (address, peer) = (*address, peer.clone());
                self.hosts
                    .update(cx, |hosts, cx| hosts.remember_peer(address, peer, cx));
            }
            StreamEvent::Notice(notice) => self.notice = Some(notice.clone()),
            StreamEvent::Ended(notice) => {
                self.notice = Some(notice.clone());
                self.stream = None;
                window.focus(&self.focus_handle, cx);
            }
        }
        cx.notify();
    }

    fn input_engaged(&self, cx: &App) -> bool {
        self.stream
            .as_ref()
            .is_some_and(|stream| stream.view.read(cx).is_engaged())
    }

    fn end_stream(&mut self, cx: &mut Context<Self>) {
        if let Some(stream) = self.stream.take() {
            stream.view.update(cx, StreamView::disconnect);
        }
    }

    fn render_navigation(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let mut tabs = Vec::with_capacity(Page::ALL.len());

        for (position, page) in Page::ALL.into_iter().enumerate() {
            tabs.push(
                option_button(("page", position), page.title(), self.page == page).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.page = page;
                        if page == Page::Hosts {
                            this.hosts.update(cx, HostsView::refresh);
                        }
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.stream {
            Some(stream) => stream.view.clone().into_any_element(),
            None => div()
                .flex()
                .flex_col()
                .size_full()
                .child(self.render_navigation(cx))
                .child(div().flex_1().min_h_0().child(self.render_page()))
                .into_any_element(),
        };

        div()
            .id("root")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &FocusNext, window, cx| window.focus_next(cx)))
            .on_action(cx.listener(|_, _: &FocusPrevious, window, cx| window.focus_prev(cx)))
            .on_action(cx.listener(|this, _: &ToggleFullscreen, window, cx| {
                if !this.input_engaged(cx) {
                    window.toggle_fullscreen();
                }
            }))
            .on_action(cx.listener(|this, _: &Quit, _, cx| {
                if this.input_engaged(cx) {
                    return;
                }
                this.end_stream(cx);
                cx.quit();
            }))
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::background())
            .text_color(theme::text())
            .child(div().flex_1().min_h_0().child(content))
            .children(
                (!(window.is_fullscreen() && self.stream.is_some())).then(|| self.render_status()),
            )
    }
}
