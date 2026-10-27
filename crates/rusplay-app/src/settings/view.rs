use std::{
    collections::{BTreeMap, HashMap},
    mem,
};

use gpui::{
    Anchor, AnyElement, Context, Entity, EventEmitter, Subscription, Window, anchored, deferred,
    div, point, prelude::*, px,
};
use rusplay_config::{Config, ConfigEnvironment, ConfigRole};

use crate::{
    notice::{Notice, NoticeRaised, error_chain},
    settings::{
        form::{FormModel, Pair, Value},
        role::{ROLES, load_baseline, role_label},
        schema::{FIELDS, Field, Kind, Section},
    },
    theme,
    widgets::{
        controls::{ButtonStyle, button, option_button, section_title, toggle},
        text_field::{TextField, TextFieldEvent},
    },
};

const SEGMENTED_LIMIT: usize = 4;
const LABEL_WIDTH: f32 = 200.0;
const INPUT_WIDTH: f32 = 300.0;
const SIDEBAR_WIDTH: f32 = 190.0;
const DROPDOWN_WIDTH: f32 = 230.0;
const DROPDOWN_MAX_HEIGHT: f32 = 340.0;
const DROPDOWN_OFFSET: f32 = 4.0;
const UNSET_LABEL: &str = "(none)";

struct PairInputs {
    key: Entity<TextField>,
    value: Entity<TextField>,
}

struct RoleState {
    saved: Config,
    form: FormModel,
    dirty: bool,
}

pub struct SettingsView {
    environment: ConfigEnvironment,
    role: ConfigRole,
    parked: Option<RoleState>,
    saved: Config,
    form: FormModel,
    section: Section,
    inputs: Vec<Option<Entity<TextField>>>,
    pair_inputs: HashMap<usize, Vec<PairInputs>>,
    pair_subscriptions: HashMap<usize, Vec<Subscription>>,
    open_choice: Option<usize>,
    errors: BTreeMap<usize, String>,
    dirty: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<NoticeRaised> for SettingsView {}

fn uses_text_input(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Integer { .. }
            | Kind::Decimal
            | Kind::Text
            | Kind::Address
            | Kind::Resolution
            | Kind::FrameRate
    )
}

fn placeholder(field: &Field) -> &'static str {
    match field.kind {
        Kind::Resolution => "1920x1080, empty for the capture size",
        Kind::FrameRate => "60 or 60000/1001",
        _ if field.optional => "not set",
        _ => "",
    }
}

impl SettingsView {
    pub fn new(
        config: Config,
        form: FormModel,
        environment: ConfigEnvironment,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut subscriptions = Vec::new();

        let mut view = Self {
            environment,
            role: ConfigRole::Client,
            parked: None,
            saved: config,
            form,
            section: Section::Video,
            inputs: Vec::new(),
            pair_inputs: HashMap::new(),
            pair_subscriptions: HashMap::new(),
            open_choice: None,
            errors: BTreeMap::new(),
            dirty: false,
            _subscriptions: Vec::new(),
        };

        for (index, field) in FIELDS.iter().enumerate() {
            if uses_text_input(field.kind) {
                let text = view.form.text(index).to_owned();
                let input = cx.new(|cx| TextField::new(&text, placeholder(field), cx));
                let subscription =
                    cx.subscribe(&input, move |this, input, event, cx| match event {
                        TextFieldEvent::Changed => this.text_edited(index, &input, cx),
                        TextFieldEvent::Submitted => this.save(cx),
                    });
                subscriptions.push(subscription);
                view.inputs.push(Some(input));
            } else {
                view.inputs.push(None);
            }

            if field.kind == Kind::Pairs {
                view.rebuild_pairs(index, cx);
            }
        }

        Self {
            _subscriptions: subscriptions,
            ..view
        }
    }

    fn text_edited(&mut self, index: usize, input: &Entity<TextField>, cx: &mut Context<Self>) {
        let text = input.read(cx).text().to_owned();
        self.form.set_text(index, text);
        self.mark_edited(index);
        cx.notify();
    }

    fn mark_edited(&mut self, index: usize) {
        self.dirty = true;

        match self.form.check_field(index) {
            Ok(()) => {
                self.errors.remove(&index);
            }
            Err(error) => {
                self.errors.insert(index, error.to_string());
            }
        }
    }

    fn choose(&mut self, index: usize, option: &str, cx: &mut Context<Self>) {
        self.form.set_text(index, option);
        self.open_choice = None;
        self.mark_edited(index);
        cx.notify();
    }

    fn flip(&mut self, index: usize, cx: &mut Context<Self>) {
        let flag = matches!(self.form.value(index), Value::Flag(true));
        self.form.set_flag(index, !flag);
        self.mark_edited(index);
        cx.notify();
    }

    fn toggle_member(&mut self, index: usize, member: &str, cx: &mut Context<Self>) {
        self.form.toggle_member(index, member);
        self.mark_edited(index);
        cx.notify();
    }

    fn rebuild_pairs(&mut self, index: usize, cx: &mut Context<Self>) {
        let pairs = self.form.pairs(index).to_vec();
        let mut inputs = Vec::with_capacity(pairs.len());
        let mut subscriptions = Vec::with_capacity(pairs.len() * 2);

        for (position, pair) in pairs.iter().enumerate() {
            let key = cx.new(|cx| TextField::new(&pair.key, "name", cx));
            let value = cx.new(|cx| TextField::new(&pair.value, "value", cx));

            for entity in [&key, &value] {
                subscriptions.push(cx.subscribe(entity, move |this, _, event, cx| {
                    if *event == TextFieldEvent::Changed {
                        this.pair_edited(index, position, cx);
                    }
                }));
            }

            inputs.push(PairInputs { key, value });
        }

        self.pair_inputs.insert(index, inputs);
        self.pair_subscriptions.insert(index, subscriptions);
    }

    fn pair_edited(&mut self, index: usize, position: usize, cx: &mut Context<Self>) {
        let Some(inputs) = self
            .pair_inputs
            .get(&index)
            .and_then(|inputs| inputs.get(position))
        else {
            return;
        };

        let pair = Pair {
            key: inputs.key.read(cx).text().to_owned(),
            value: inputs.value.read(cx).text().to_owned(),
        };

        self.form.set_pair(index, position, pair);
        self.mark_edited(index);
        cx.notify();
    }

    fn add_pair(&mut self, index: usize, cx: &mut Context<Self>) {
        self.form.add_pair(index);
        self.rebuild_pairs(index, cx);
        self.dirty = true;
        cx.notify();
    }

    fn remove_pair(&mut self, index: usize, position: usize, cx: &mut Context<Self>) {
        self.form.remove_pair(index, position);
        self.rebuild_pairs(index, cx);
        self.mark_edited(index);
        cx.notify();
    }

    fn reset(&mut self, cx: &mut Context<Self>) {
        match FormModel::from_config(&self.saved) {
            Ok(form) => self.form = form,
            Err(error) => {
                cx.emit(NoticeRaised(Notice::Failure(error_chain(&error))));
                return;
            }
        }

        self.sync_inputs(cx);
        self.dirty = false;
        cx.emit(NoticeRaised(Notice::Info(
            "Reverted to the saved settings".to_owned(),
        )));
        cx.notify();
    }

    fn sync_inputs(&mut self, cx: &mut Context<Self>) {
        for (index, input) in self.inputs.iter().enumerate() {
            if let Some(input) = input {
                let text = self.form.text(index).to_owned();
                input.update(cx, |input, cx| input.set_text(&text, cx));
            }
        }

        let pair_fields: Vec<usize> = self.pair_inputs.keys().copied().collect();
        for index in pair_fields {
            self.rebuild_pairs(index, cx);
        }

        self.errors.clear();
        self.open_choice = None;
    }

    fn switch_role(&mut self, role: ConfigRole, cx: &mut Context<Self>) {
        if role == self.role {
            return;
        }

        let incoming = match self.parked.take() {
            Some(state) => state,
            None => match self.load_role(role) {
                Ok(state) => state,
                Err(message) => {
                    cx.emit(NoticeRaised(Notice::Failure(message)));
                    return;
                }
            },
        };

        self.parked = Some(RoleState {
            saved: mem::replace(&mut self.saved, incoming.saved),
            form: mem::replace(&mut self.form, incoming.form),
            dirty: mem::replace(&mut self.dirty, incoming.dirty),
        });
        self.role = role;
        self.sync_inputs(cx);
        cx.notify();
    }

    fn load_role(&self, role: ConfigRole) -> Result<RoleState, String> {
        let failure = |error: &dyn std::error::Error| {
            format!(
                "Could not load the {} config: {}",
                role_label(role).to_lowercase(),
                error_chain(error)
            )
        };

        let saved = load_baseline(&self.environment, role).map_err(|error| failure(&error))?;
        let form = FormModel::from_config(&saved).map_err(|error| failure(&error))?;

        Ok(RoleState {
            saved,
            form,
            dirty: false,
        })
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        self.errors.clear();

        let config = match self.form.build() {
            Ok(config) => config,
            Err(issues) => {
                let mut unplaced = Vec::new();
                for issue in &issues {
                    match issue.field() {
                        Some(field) => {
                            self.errors.insert(field, issue.message());
                        }
                        None => unplaced.push(issue.to_string()),
                    }
                }

                if let Some((&first, _)) = self.errors.first_key_value() {
                    self.section = FIELDS[first].section;
                }

                let mut message = format!("{} setting(s) need attention", issues.len());
                for text in unplaced {
                    message.push_str(": ");
                    message.push_str(&text);
                }

                cx.emit(NoticeRaised(Notice::Failure(message)));
                cx.notify();
                return;
            }
        };

        let written = self
            .environment
            .config_path(self.role)
            .and_then(|path| config.save(&path).map(|()| path));

        match written {
            Ok(path) => {
                self.saved = config;
                self.dirty = false;
                cx.emit(NoticeRaised(Notice::Info(format!(
                    "Saved {}",
                    path.display()
                ))));
            }
            Err(error) => cx.emit(NoticeRaised(Notice::Failure(format!(
                "Could not save: {}",
                error_chain(&error)
            )))),
        }

        cx.notify();
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let mut items = Vec::with_capacity(Section::ALL.len());

        for (position, section) in Section::ALL.into_iter().enumerate() {
            let selected = self.section == section;
            let background = if selected {
                theme::surface_raised()
            } else {
                theme::background()
            };

            items.push(
                div()
                    .id(("section", position))
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .cursor_pointer()
                    .bg(background)
                    .text_color(if selected {
                        theme::text()
                    } else {
                        theme::text_muted()
                    })
                    .hover(|style| style.bg(theme::surface_hover()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.section = section;
                        this.open_choice = None;
                        cx.notify();
                    }))
                    .child(section.title()),
            );
        }

        div()
            .flex()
            .flex_col()
            .gap_1()
            .w(px(SIDEBAR_WIDTH))
            .flex_none()
            .p_3()
            .border_r_1()
            .border_color(theme::border())
            .child(
                div()
                    .px_3()
                    .pb_2()
                    .text_xl()
                    .text_color(theme::text())
                    .child("Settings"),
            )
            .children(items)
    }

    fn render_control(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let field = &FIELDS[index];

        match field.kind {
            Kind::Flag => {
                let on = matches!(self.form.value(index), Value::Flag(true));
                toggle(("toggle", index), on)
                    .on_click(cx.listener(move |this, _, _, cx| this.flip(index, cx)))
                    .into_any_element()
            }
            Kind::Choice(options) if options.len() <= SEGMENTED_LIMIT && !field.optional => {
                self.render_segmented(index, options, cx)
            }
            Kind::Choice(options) => self.render_dropdown(index, options, field.optional, cx),
            Kind::MultiChoice(options) => self.render_chips(index, options, cx),
            Kind::Pairs => self.render_pairs(index, cx),
            Kind::Integer { .. }
            | Kind::Decimal
            | Kind::Text
            | Kind::Address
            | Kind::Resolution
            | Kind::FrameRate => self.render_text_input(index, cx),
        }
    }

    fn render_text_input(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(input) = self.inputs[index].clone() else {
            return div().into_any_element();
        };

        let invalid = self.errors.contains_key(&index);
        input.update(cx, |input, cx| input.set_invalid(invalid, cx));

        div().w(px(INPUT_WIDTH)).child(input).into_any_element()
    }

    fn render_segmented(
        &mut self,
        index: usize,
        options: &'static [&'static str],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = self.form.text(index).to_owned();
        let mut buttons = Vec::with_capacity(options.len());

        for (position, option) in options.iter().copied().enumerate() {
            buttons.push(
                option_button(
                    ("choice", index * 100 + position),
                    option,
                    current == option,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.choose(index, option, cx))),
            );
        }

        div()
            .flex()
            .flex_wrap()
            .gap_1()
            .children(buttons)
            .into_any_element()
    }

    fn render_chips(
        &mut self,
        index: usize,
        options: &'static [&'static str],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected: Vec<String> = match self.form.value(index) {
            Value::Set(members) => members.clone(),
            Value::Flag(_) | Value::Text(_) | Value::Pairs(_) => Vec::new(),
        };
        let mut chips = Vec::with_capacity(options.len());

        for (position, option) in options.iter().copied().enumerate() {
            chips.push(
                option_button(
                    ("chip", index * 100 + position),
                    option,
                    selected.iter().any(|member| member == option),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_member(index, option, cx))),
            );
        }

        div()
            .flex()
            .flex_wrap()
            .gap_1()
            .max_w(px(560.0))
            .children(chips)
            .into_any_element()
    }

    fn render_dropdown(
        &mut self,
        index: usize,
        options: &'static [&'static str],
        optional: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = self.form.text(index).to_owned();
        let shown = if current.is_empty() {
            UNSET_LABEL.to_owned()
        } else {
            current.clone()
        };
        let open = self.open_choice == Some(index);

        let mut entries: Vec<&'static str> = Vec::with_capacity(options.len() + 1);
        if optional {
            entries.push("");
        }
        entries.extend(options.iter().copied());

        let list = div()
            .id(("dropdown-list", index))
            .w(px(DROPDOWN_WIDTH))
            .max_h(px(DROPDOWN_MAX_HEIGHT))
            .overflow_y_scroll()
            .occlude()
            .flex()
            .flex_col()
            .gap_1()
            .p_1()
            .rounded_md()
            .border_1()
            .border_color(theme::border())
            .bg(theme::surface())
            .shadow_lg()
            .on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                if this.open_choice == Some(index) {
                    this.open_choice = None;
                    cx.notify();
                }
            }))
            .children(entries.into_iter().enumerate().map(|(position, option)| {
                let label = if option.is_empty() {
                    UNSET_LABEL
                } else {
                    option
                };
                option_button(
                    ("dropdown-option", index * 100 + position),
                    label,
                    current == option,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.choose(index, option, cx)))
            }));

        div()
            .relative()
            .child(
                option_button(("dropdown", index), format!("{shown}  \u{25be}"), false)
                    .min_w(px(DROPDOWN_WIDTH))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_choice = if this.open_choice == Some(index) {
                            None
                        } else {
                            Some(index)
                        };
                        cx.notify();
                    })),
            )
            .when(open, |this| {
                this.child(
                    deferred(
                        anchored()
                            .anchor(Anchor::TopLeft)
                            .offset(point(px(0.0), px(DROPDOWN_OFFSET)))
                            .snap_to_window_with_margin(px(8.0))
                            .child(list),
                    )
                    .priority(1),
                )
            })
            .into_any_element()
    }

    fn render_pairs(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let mut rows = Vec::new();

        if let Some(inputs) = self.pair_inputs.get(&index) {
            for (position, pair) in inputs.iter().enumerate() {
                rows.push(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().w(px(180.0)).child(pair.key.clone()))
                        .child(div().text_color(theme::text_muted()).child("="))
                        .child(div().w(px(220.0)).child(pair.value.clone()))
                        .child(
                            button(
                                ("remove-pair", index * 1000 + position),
                                "Remove",
                                ButtonStyle::Quiet,
                                true,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.remove_pair(index, position, cx);
                                },
                            )),
                        ),
                );
            }
        }

        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(rows)
            .child(
                div().flex().child(
                    button(
                        ("add-pair", index),
                        "Add parameter",
                        ButtonStyle::Secondary,
                        true,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.add_pair(index, cx))),
                ),
            )
            .into_any_element()
    }

    fn render_field(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let field = &FIELDS[index];
        let control = self.render_control(index, cx);
        let error = self.errors.get(&index).cloned();

        div()
            .flex()
            .flex_col()
            .gap_1()
            .py_2()
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_4()
                    .child(
                        div()
                            .w(px(LABEL_WIDTH))
                            .flex_none()
                            .pt(px(6.0))
                            .text_sm()
                            .text_color(theme::text())
                            .child(field.label),
                    )
                    .child(control)
                    .when(!field.unit.is_empty(), |this| {
                        this.child(
                            div()
                                .pt(px(6.0))
                                .text_sm()
                                .text_color(theme::text_muted())
                                .child(field.unit),
                        )
                    }),
            )
            .when_some(error, |this, error| {
                this.child(
                    div()
                        .ml(px(LABEL_WIDTH + 16.0))
                        .text_xs()
                        .text_color(theme::danger())
                        .child(error),
                )
            })
            .into_any_element()
    }

    fn render_roles(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let mut tabs = Vec::with_capacity(ROLES.len());

        for (position, role) in ROLES.into_iter().enumerate() {
            let unsaved = if role == self.role {
                self.dirty
            } else {
                self.parked.as_ref().is_some_and(|state| state.dirty)
            };
            let label = if unsaved {
                format!("{} config (unsaved)", role_label(role))
            } else {
                format!("{} config", role_label(role))
            };

            tabs.push(
                option_button(("role", position), label, role == self.role)
                    .on_click(cx.listener(move |this, _, _, cx| this.switch_role(role, cx))),
            );
        }

        div()
            .flex()
            .items_center()
            .gap_2()
            .px_6()
            .py_3()
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .pr_2()
                    .text_color(theme::text_muted())
                    .child("Editing"),
            )
            .children(tabs)
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let location = self
            .environment
            .config_path(self.role)
            .map_or_else(|error| error.to_string(), |path| path.display().to_string());

        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .px_6()
            .py_3()
            .border_t_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child(location),
                    )
                    .when(self.dirty, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme::accent())
                                .child("Unsaved changes"),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .gap_2()
                    .child(
                        button("settings-reset", "Revert", ButtonStyle::Quiet, self.dirty)
                            .on_click(cx.listener(|this, _, _, cx| this.reset(cx))),
                    )
                    .child(
                        button("settings-save", "Save", ButtonStyle::Primary, true)
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    ),
            )
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let visible = self.form.visible_fields(self.section);
        let mut rows: Vec<AnyElement> = Vec::with_capacity(visible.len() * 2);
        let mut group = "";

        for index in visible {
            let field = &FIELDS[index];
            if field.group != group {
                group = field.group;
                rows.push(section_title(group).into_any_element());
            }
            rows.push(self.render_field(index, cx));
        }

        let sidebar = self.render_sidebar(cx);
        let footer = self.render_footer(cx);
        let roles = self.render_roles(cx);

        div().flex().size_full().child(sidebar).child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(roles)
                .child(
                    div()
                        .id("settings-fields")
                        .flex_1()
                        .overflow_y_scroll()
                        .px_6()
                        .pb_6()
                        .child(div().flex().flex_col().children(rows)),
                )
                .child(footer),
        )
    }
}
