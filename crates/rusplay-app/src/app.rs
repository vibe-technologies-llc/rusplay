use gpui::{
    AnyElement, App, Context, Div, Entity, FocusHandle, Focusable, FontWeight, KeyBinding,
    MouseButton, Subscription, Window, actions, div, prelude::*, px,
};
use rusplay_client::HostBook;
use rusplay_config::{Config, ConfigEnvironment, ConfigRole};

use crate::{
    chrome::{self, Frame},
    cli::Launch,
    host_page::{HostLaunchPreference, HostPageView},
    hosts::{ConnectRequested, HostsView},
    notice::{Notice, NoticeRaised, error_chain},
    session,
    settings::{FormModel, HostSettingsSaved, SettingsView},
    stream::{StreamEvent, StreamView},
    theme,
    widgets::controls::{tab, tab_strip},
};

const TITLEBAR_HEIGHT: f32 = 40.0;
const STATUS_HEIGHT: f32 = 28.0;
const KEY_HINTS: &str = "Tab to move \u{00b7} F11 fullscreen \u{00b7} Ctrl+Q quit";

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
    Hosting,
    Settings,
}

impl Page {
    const ALL: [Self; 3] = [Self::Hosts, Self::Hosting, Self::Settings];

    const fn title(self) -> &'static str {
        match self {
            Self::Hosts => "Hosts",
            Self::Hosting => "Host this PC",
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
    abandoned: Vec<ActiveStream>,
    exit_on_disconnect: bool,
    hosts: Entity<HostsView>,
    hosting: Entity<HostPageView>,
    settings: Option<Entity<SettingsView>>,
    notice: Option<Notice>,
    titlebar_drag: bool,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl RusplayApp {
    pub fn new(
        environment: ConfigEnvironment,
        launch: Launch,
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

        let hosting = cx.new(|cx| HostPageView::new(environment.clone(), cx));
        subscriptions.extend(host_subscriptions(&hosting, cx));

        let app = cx.weak_entity();
        window.on_window_should_close(cx, move |_, cx| {
            app.update(cx, |this, cx| {
                this.end_stream(cx);
                this.end_hosting(cx);
            })
            .ok();
            true
        });

        let settings = match FormModel::from_config(&config, ConfigRole::Client) {
            Ok(form) => {
                let settings =
                    cx.new(|cx| SettingsView::new(config, form, environment.clone(), cx));
                subscriptions.push(
                    cx.subscribe(&settings, |this, _, event: &NoticeRaised, cx| {
                        this.notice = Some(event.0.clone());
                        cx.notify();
                    }),
                );
                subscriptions.push(cx.subscribe(
                    &settings,
                    |this, _, _: &HostSettingsSaved, cx| {
                        this.hosting.update(cx, HostPageView::apply_saved_settings);
                    },
                ));
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

        hosting.update(cx, HostPageView::start_if_requested);

        let app = Self {
            page: Page::Hosts,
            environment,
            stream: None,
            abandoned: Vec::new(),
            exit_on_disconnect: launch.exit_on_disconnect,
            hosts,
            hosting,
            settings,
            notice,
            titlebar_drag: false,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        };

        if let Some(target) = launch.connect {
            cx.defer_in(window, move |this, _, cx| {
                this.hosts
                    .update(cx, |hosts, cx| hosts.request_connection(&target, cx));
            });
        }

        app
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
                let message = format!("Could not open the stream view: {}", error_chain(&error));
                if self.exit_on_disconnect {
                    eprintln!("rusplay: {message}");
                    cx.quit();
                }
                self.notice = Some(Notice::Failure(message));
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
            |this, source, event: &StreamEvent, window, cx| {
                this.stream_event(source, event, window, cx);
            },
        );
        window.focus(&stream.focus_handle(cx), cx);

        self.stream = Some(ActiveStream {
            view: stream,
            _events: events,
        });
        self.notice = Some(Notice::Info(format!("Connecting to {host_name}")));
        cx.notify();
    }

    fn stream_event(
        &mut self,
        source: &Entity<StreamView>,
        event: &StreamEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self
            .stream
            .as_ref()
            .is_some_and(|active| active.view == *source);

        match event {
            StreamEvent::Paired { host, name, peer } => {
                let (host, peer) = (*host, peer.clone());
                let remembered = self
                    .hosts
                    .update(cx, |hosts, cx| hosts.remember_peer(host, peer, cx));
                if !remembered {
                    self.notice = Some(Notice::Failure(format!(
                        "Paired with {name}, but it is no longer in the host list, so the pairing was not saved"
                    )));
                }
            }
            StreamEvent::Notice(notice) => self.notice = Some(notice.clone()),
            StreamEvent::Abandoned if current => {
                self.abandoned.extend(self.stream.take());
                window.focus(&self.focus_handle, cx);
            }
            StreamEvent::Ended(notice) if current => {
                self.notice = Some(notice.clone());
                self.stream = None;
                window.focus(&self.focus_handle, cx);
            }
            StreamEvent::Abandoned | StreamEvent::Ended(_) => {
                self.abandoned.retain(|active| active.view != *source);
            }
        }

        if let StreamEvent::Ended(notice) = event
            && self.exit_on_disconnect
        {
            if let Notice::Failure(message) = notice {
                eprintln!("rusplay: {message}");
            }
            cx.quit();
        }
        cx.notify();
    }

    fn input_engaged(&self, cx: &App) -> bool {
        self.stream
            .as_ref()
            .is_some_and(|stream| stream.view.read(cx).is_engaged())
    }

    fn end_stream(&mut self, cx: &mut Context<Self>) {
        if let Some(stream) = &self.stream {
            stream.view.update(cx, StreamView::disconnect);
        }
    }

    fn end_hosting(&mut self, cx: &mut Context<Self>) {
        self.hosting.update(cx, |hosting, _| hosting.shutdown());
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> Div {
        let mut tabs = Vec::with_capacity(Page::ALL.len());
        for (position, page) in Page::ALL.into_iter().enumerate() {
            tabs.push(
                tab(("page", position), page.title(), self.page == page).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.page = page;
                        match page {
                            Page::Hosts => this.hosts.update(cx, HostsView::refresh),
                            Page::Hosting => this.hosting.update(cx, HostPageView::refresh),
                            Page::Settings => {}
                        }
                        cx.notify();
                    },
                )),
            );
        }

        div().occlude().child(tab_strip(tabs))
    }

    fn render_titlebar(
        &self,
        frame: Frame,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let tabs = self.stream.is_none().then(|| self.render_tabs(cx));

        let brand = div()
            .flex()
            .flex_1()
            .flex_basis(px(0.0))
            .items_center()
            .child(
                div()
                    .text_sm()
                    .text_color(theme::text())
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Rusplay"),
            );

        let controls = div()
            .flex()
            .flex_1()
            .flex_basis(px(0.0))
            .justify_end()
            .when(frame.is_client(), |this| {
                this.child(chrome::window_controls(
                    window,
                    cx.listener(|this, _, _, cx| {
                        this.end_stream(cx);
                        this.end_hosting(cx);
                        cx.quit();
                    }),
                ))
            });

        div()
            .id("titlebar")
            .flex()
            .flex_none()
            .items_center()
            .gap_4()
            .pl_4()
            .pr_1()
            .h(px(TITLEBAR_HEIGHT))
            .border_b_1()
            .border_color(theme::rule())
            .bg(theme::chrome())
            .when(frame.rounded(), |this| {
                this.rounded_t(chrome::corner_radius())
            })
            .when(frame.is_client(), |this| {
                this.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, _| this.titlebar_drag = true),
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, _, _| this.titlebar_drag = false),
                )
                .on_mouse_move(cx.listener(|this, _, window, _| {
                    if this.titlebar_drag {
                        this.titlebar_drag = false;
                        window.start_window_move();
                    }
                }))
                .on_click(|event, window, _| {
                    if event.click_count() == 2 {
                        window.zoom_window();
                    }
                })
                .on_mouse_down(MouseButton::Right, |event, window, _| {
                    window.show_window_menu(event.position);
                })
            })
            .child(brand)
            .children(tabs)
            .child(controls)
    }

    fn render_page(&self) -> AnyElement {
        match (self.page, &self.settings) {
            (Page::Hosts, _) => self.hosts.clone().into_any_element(),
            (Page::Hosting, _) => self.hosting.clone().into_any_element(),
            (Page::Settings, Some(settings)) => settings.clone().into_any_element(),
            (Page::Settings, None) => div()
                .p_6()
                .text_color(theme::danger())
                .child("Settings could not be built from the current configuration.")
                .into_any_element(),
        }
    }

    fn render_status(&self, frame: Frame) -> impl IntoElement + use<> {
        let (color, text) = match &self.notice {
            Some(Notice::Info(text)) => (theme::text_muted(), text.clone()),
            Some(Notice::Failure(text)) => (theme::danger(), text.clone()),
            None => (theme::text_muted(), String::new()),
        };

        div()
            .id("status")
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap_4()
            .px_4()
            .min_h(px(STATUS_HEIGHT))
            .border_t_1()
            .border_color(theme::rule())
            .bg(theme::chrome())
            .when(frame.rounded(), |this| {
                this.rounded_b(chrome::corner_radius())
            })
            .text_xs()
            .child(div().min_w_0().truncate().text_color(color).child(text))
            .child(
                div()
                    .flex_none()
                    .text_color(theme::text_faint())
                    .child(KEY_HINTS),
            )
    }
}

fn host_subscriptions(
    hosting: &Entity<HostPageView>,
    cx: &mut Context<RusplayApp>,
) -> Vec<Subscription> {
    vec![
        cx.subscribe(hosting, |this, _, event: &NoticeRaised, cx| {
            this.notice = Some(event.0.clone());
            cx.notify();
        }),
        cx.subscribe(hosting, |this, _, event: &HostLaunchPreference, cx| {
            let HostLaunchPreference(enabled) = *event;
            if let Some(settings) = &this.settings {
                settings.update(cx, |settings, cx| {
                    settings.note_host_launch(enabled);
                    cx.notify();
                });
            }
        }),
    ]
}

impl Focusable for RusplayApp {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RusplayApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let frame = Frame::of(window);
        let immersive = window.is_fullscreen() && self.stream.is_some();

        let content = match &self.stream {
            Some(stream) => stream.view.clone().into_any_element(),
            None => self.render_page(),
        };

        let body = div()
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
                this.end_stream(cx);
                this.end_hosting(cx);
                cx.quit();
            }))
            .flex()
            .flex_col()
            .bg(theme::canvas())
            .text_color(theme::text())
            .children((!immersive).then(|| self.render_titlebar(frame, window, cx)))
            .child(div().flex_1().min_h_0().child(content))
            .children((!immersive).then(|| self.render_status(frame)));

        chrome::frame(window, body)
    }
}
