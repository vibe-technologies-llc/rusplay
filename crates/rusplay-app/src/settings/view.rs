use std::{
    collections::{BTreeMap, HashMap},
    mem,
};

use gpui::{
    Anchor, AnyElement, Context, Entity, EventEmitter, FontWeight, ScrollHandle, Subscription,
    Task, Window, anchored, deferred, div, point, prelude::*, px,
};
use rusplay_config::{
    Config, ConfigEnvironment, ConfigRole, VideoDecoderConfig, VideoEncoderConfig,
};
use rusplay_core::FormatSet;

use crate::{
    notice::{Notice, NoticeRaised, error_chain},
    settings::{
        display::DisplayColorDefaults,
        form::{FormModel, Pair, Value},
        labels::{choice_label, field_matches, section_summary, segmented_choice},
        outlook::{self, OutlookLine},
        primaries,
        role::{ROLES, load_baseline, role_label},
        schema::{FIELDS, Field, Kind, Section},
    },
    theme,
    widgets::{
        controls::{
            ButtonStyle, Tone, button, group_heading, menu_row, option_button, panel, segment,
            segmented, select_trigger, selection_bar, state_text, toggle,
        },
        text_field::{TextField, TextFieldEvent},
    },
};

const LABEL_WIDTH: f32 = 200.0;
const INPUT_WIDTH: f32 = 280.0;
const PRIMARIES_INPUT_WIDTH: f32 = 320.0;
const SIDEBAR_WIDTH: f32 = 216.0;
const SIDEBAR_ITEM_HEIGHT: f32 = 30.0;
const LABEL_TOP: f32 = 6.0;
const MARK_SIZE: f32 = 6.0;
const DROPDOWN_WIDTH: f32 = 240.0;
const DROPDOWN_MAX_HEIGHT: f32 = 340.0;
const DROPDOWN_OFFSET: f32 = 4.0;
const UNSET_LABEL: &str = "(none)";
const SEARCH_PLACEHOLDER: &str = "Search";

struct PairInputs {
    key: Entity<TextField>,
    value: Entity<TextField>,
}

struct RoleState {
    saved: Config,
    form: FormModel,
    dirty: bool,
}

#[derive(Clone, PartialEq)]
enum DeviceKey {
    Encoder(VideoEncoderConfig),
    Decoder(VideoDecoderConfig),
}

struct Detected {
    backend: String,
    formats: FormatSet,
}

struct HardwareProbe {
    key: DeviceKey,
    formats: Option<Result<Detected, String>>,
}

enum ScrollTo {
    Top,
    Field(usize),
}

#[derive(Debug, Clone, Copy)]
pub struct HostSettingsSaved;

pub struct SettingsView {
    environment: ConfigEnvironment,
    role: ConfigRole,
    parked: Option<RoleState>,
    saved: Config,
    form: FormModel,
    defaults: Option<FormModel>,
    color_defaults: Option<DisplayColorDefaults>,
    section: Section,
    search: Entity<TextField>,
    query: String,
    scroll: ScrollHandle,
    scroll_to: Option<ScrollTo>,
    inputs: Vec<Option<Entity<TextField>>>,
    pair_inputs: HashMap<usize, Vec<PairInputs>>,
    pair_subscriptions: HashMap<usize, Vec<Subscription>>,
    open_choice: Option<usize>,
    errors: BTreeMap<usize, String>,
    dirty: bool,
    probe_generation: u64,
    probe: Option<HardwareProbe>,
    probe_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<NoticeRaised> for SettingsView {}
impl EventEmitter<HostSettingsSaved> for SettingsView {}

fn uses_text_input(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Integer { .. }
            | Kind::Decimal
            | Kind::Text
            | Kind::Address
            | Kind::Resolution
            | Kind::FrameRate
            | Kind::Primaries
    )
}

fn placeholder(field: &Field, defaults: Option<&DisplayColorDefaults>) -> String {
    if matches!(field.kind, Kind::Integer { .. } | Kind::Decimal)
        && let Some(label) = defaults.and_then(|defaults| defaults.label(field.path))
    {
        return label.to_owned();
    }

    match field.kind {
        Kind::Resolution => "1920x1080, empty for the capture size",
        Kind::FrameRate => "auto, 60 or 60000/1001",
        Kind::Primaries => "custom RX,RY GX,GY BX,BY WX,WY",
        _ if field.optional => "not set",
        _ => "",
    }
    .to_owned()
}

impl SettingsView {
    pub fn new(
        config: Config,
        form: FormModel,
        environment: ConfigEnvironment,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut subscriptions = Vec::new();

        let color_defaults = DisplayColorDefaults::query();
        let search = cx.new(|cx| TextField::new("", SEARCH_PLACEHOLDER, cx));
        subscriptions.push(cx.subscribe(&search, |this, input, event, cx| {
            if *event == TextFieldEvent::Changed {
                this.search_edited(&input, cx);
            }
        }));
        let mut view = Self {
            environment,
            role: ConfigRole::Client,
            parked: None,
            saved: config,
            form,
            defaults: FormModel::defaults(ConfigRole::Client).ok(),
            color_defaults,
            section: Section::Video,
            search,
            query: String::new(),
            scroll: ScrollHandle::new(),
            scroll_to: None,
            inputs: Vec::new(),
            pair_inputs: HashMap::new(),
            pair_subscriptions: HashMap::new(),
            open_choice: None,
            errors: BTreeMap::new(),
            dirty: false,
            probe_generation: 0,
            probe: None,
            probe_task: None,
            _subscriptions: Vec::new(),
        };

        for (index, field) in FIELDS.iter().enumerate() {
            if uses_text_input(field.kind) {
                let text = view.form.text(index).to_owned();
                let input = cx.new(|cx| {
                    TextField::new(&text, &placeholder(field, view.color_defaults.as_ref()), cx)
                });
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

        let mut view = Self {
            _subscriptions: subscriptions,
            ..view
        };
        view.sync_hardware(cx);
        view
    }

    fn text_edited(&mut self, index: usize, input: &Entity<TextField>, cx: &mut Context<Self>) {
        let text = input.read(cx).text().to_owned();
        self.form.set_text(index, text);
        self.mark_edited(index);
        self.sync_hardware(cx);
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

    fn search_edited(&mut self, input: &Entity<TextField>, cx: &mut Context<Self>) {
        input.read(cx).text().clone_into(&mut self.query);
        self.open_choice = None;
        self.scroll_to = Some(ScrollTo::Top);
        cx.notify();
    }

    fn clear_search(&mut self, cx: &mut Context<Self>) {
        if self.query.is_empty() {
            return;
        }

        self.query.clear();
        self.search.update(cx, |field, cx| field.set_text("", cx));
        self.open_choice = None;
        self.scroll_to = Some(ScrollTo::Top);
        cx.notify();
    }

    fn show_section(&mut self, section: Section, cx: &mut Context<Self>) {
        self.section = section;
        self.open_choice = None;
        self.scroll_to = Some(ScrollTo::Top);

        if !self.query.is_empty() {
            self.query.clear();
            self.search.update(cx, |field, cx| field.set_text("", cx));
        }

        cx.notify();
    }

    fn searching(&self) -> bool {
        !self.query.trim().is_empty()
    }

    fn listed_fields(&self) -> Vec<usize> {
        if !self.searching() {
            return self.form.visible_fields(self.section);
        }

        let query = self.query.trim();

        Section::ALL
            .into_iter()
            .flat_map(|section| self.form.visible_fields(section))
            .filter(|index| field_matches(&FIELDS[*index], query))
            .collect()
    }

    fn section_has_error(&self, section: Section) -> bool {
        self.errors
            .keys()
            .any(|index| FIELDS[*index].section == section)
    }

    fn section_match_count(&self, section: Section) -> usize {
        if !self.searching() {
            return 0;
        }

        let query = self.query.trim();

        self.form
            .visible_fields(section)
            .into_iter()
            .filter(|index| field_matches(&FIELDS[*index], query))
            .count()
    }

    fn choose(&mut self, index: usize, option: &str, cx: &mut Context<Self>) {
        let text = if FIELDS[index].kind == Kind::Primaries {
            primaries::choice(self.form.text(index), option)
        } else {
            option.to_owned()
        };
        if let Some(Some(input)) = self.inputs.get(index) {
            input.update(cx, |input, cx| input.set_text(&text, cx));
        }

        self.form.set_text(index, text);
        self.open_choice = None;
        self.mark_edited(index);
        self.sync_hardware(cx);
        cx.notify();
    }

    fn flip(&mut self, index: usize, cx: &mut Context<Self>) {
        let flag = matches!(self.form.value(index), Value::Flag(true));
        self.form.set_flag(index, !flag);
        self.mark_edited(index);
        self.sync_hardware(cx);
        cx.notify();
    }

    fn toggle_member(&mut self, index: usize, member: &str, cx: &mut Context<Self>) {
        self.form.toggle_member(index, member);
        self.mark_edited(index);
        self.sync_hardware(cx);
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
        self.sync_hardware(cx);
        cx.notify();
    }

    fn add_pair(&mut self, index: usize, cx: &mut Context<Self>) {
        self.form.add_pair(index);
        self.rebuild_pairs(index, cx);
        self.dirty = true;
        self.sync_hardware(cx);
        cx.notify();
    }

    fn remove_pair(&mut self, index: usize, position: usize, cx: &mut Context<Self>) {
        self.form.remove_pair(index, position);
        self.rebuild_pairs(index, cx);
        self.mark_edited(index);
        self.sync_hardware(cx);
        cx.notify();
    }

    fn reset(&mut self, cx: &mut Context<Self>) {
        match FormModel::from_config(&self.saved, self.role) {
            Ok(form) => self.form = form,
            Err(error) => {
                cx.emit(NoticeRaised(Notice::Failure(error_chain(&error))));
                return;
            }
        }

        self.sync_inputs(cx);
        self.dirty = false;
        self.sync_hardware(cx);
        self.settle_section();
        cx.emit(NoticeRaised(Notice::Info(
            "Reverted to the saved settings".to_owned(),
        )));
        cx.notify();
    }

    fn section_is_default(&self) -> bool {
        self.defaults
            .as_ref()
            .is_none_or(|defaults| self.form.section_matches(defaults, self.section))
    }

    fn recheck_fields(&mut self) {
        self.errors = (0..FIELDS.len())
            .filter(|&index| self.form.is_visible(index))
            .filter_map(|index| {
                let error = self.form.check_field(index).err()?;
                Some((index, error.to_string()))
            })
            .collect();
    }

    fn reset_section(&mut self, cx: &mut Context<Self>) {
        if self.section_is_default() {
            return;
        }
        let Some(defaults) = &self.defaults else {
            return;
        };

        self.form.adopt_section(defaults, self.section);
        self.sync_inputs(cx);
        self.recheck_fields();
        self.dirty = self
            .form
            .build()
            .map_or(true, |config| config != self.saved);
        self.sync_hardware(cx);
        self.scroll_to = Some(ScrollTo::Top);
        cx.emit(NoticeRaised(Notice::Info(format!(
            "{} reset to defaults, not saved yet",
            self.section.title()
        ))));
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
        self.defaults = FormModel::defaults(role).ok();
        self.sync_inputs(cx);
        self.settle_section();
        self.sync_hardware(cx);
        self.scroll_to = Some(ScrollTo::Top);
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
        let form = FormModel::from_config(&saved, role).map_err(|error| failure(&error))?;

        Ok(RoleState {
            saved,
            form,
            dirty: false,
        })
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if !self.dirty {
            return;
        }

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
                    self.query.clear();
                    self.search.update(cx, |field, cx| field.set_text("", cx));
                    self.scroll_to = Some(ScrollTo::Field(first));
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

        if config == self.saved {
            self.dirty = false;
            cx.notify();
            return;
        }

        let host_settings = self.role == ConfigRole::Host;
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
                if host_settings {
                    cx.emit(HostSettingsSaved);
                }
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

        let searching = self.searching();

        for (position, section) in Section::ALL.into_iter().enumerate() {
            if self.form.visible_fields(section).is_empty() {
                continue;
            }

            let selected = !searching && self.section == section;
            let matches = self.section_match_count(section);
            let errored = self.section_has_error(section);

            items.push(
                div()
                    .id(("section", position))
                    .relative()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(SIDEBAR_ITEM_HEIGHT))
                    .pl_3()
                    .pr_2()
                    .rounded(theme::radius_control())
                    .cursor_pointer()
                    .text_sm()
                    .map(|this| {
                        if selected {
                            this.bg(theme::raised())
                                .text_color(theme::text())
                                .font_weight(FontWeight::MEDIUM)
                                .child(selection_bar())
                        } else {
                            this.text_color(theme::text_muted())
                                .hover(|style| style.bg(theme::raised()).text_color(theme::text()))
                        }
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.show_section(section, cx)))
                    .child(div().flex_1().min_w_0().truncate().child(section.title()))
                    .when(searching && matches > 0, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .font_features(theme::tabular())
                                .text_color(theme::text_faint())
                                .child(matches.to_string()),
                        )
                    })
                    .when(errored, |this| {
                        this.child(
                            div()
                                .size(px(MARK_SIZE))
                                .flex_none()
                                .rounded_full()
                                .bg(theme::danger()),
                        )
                    }),
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
            .border_color(theme::rule())
            .bg(theme::chrome())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .px_1()
                    .pt_1()
                    .pb_3()
                    .child(
                        div()
                            .px_2()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child("Settings"),
                    )
                    .child(div().w_full().child(self.search.clone())),
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
            Kind::Choice(options) if segmented_choice(field.optional, options) => {
                self.render_segmented(index, options, cx)
            }
            Kind::Choice(options) => self.render_dropdown(index, options, field.optional, cx),
            Kind::Primaries => self.render_primaries(index, cx),
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

    fn unset_label(&self, index: usize) -> String {
        self.color_defaults
            .as_ref()
            .and_then(|defaults| defaults.label(FIELDS[index].path))
            .unwrap_or(UNSET_LABEL)
            .to_owned()
    }

    fn selected_choice(&self, index: usize) -> String {
        let text = self.form.text(index);

        if FIELDS[index].kind == Kind::Primaries {
            primaries::selected(text).to_owned()
        } else {
            text.to_owned()
        }
    }

    fn render_primaries(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let selector = self.render_dropdown(index, primaries::CHOICES, FIELDS[index].optional, cx);
        let mut row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(selector);

        if primaries::is_custom(self.form.text(index))
            && let Some(input) = self.inputs[index].clone()
        {
            let invalid = self.errors.contains_key(&index);
            input.update(cx, |input, cx| input.set_invalid(invalid, cx));
            row = row.child(div().w(px(PRIMARIES_INPUT_WIDTH)).child(input));
        }

        row.into_any_element()
    }

    fn render_segmented(
        &mut self,
        index: usize,
        options: &'static [&'static str],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = self.selected_choice(index);
        let mut buttons = Vec::with_capacity(options.len());

        for (position, option) in options.iter().copied().enumerate() {
            buttons.push(
                segment(
                    ("choice", index * 100 + position),
                    choice_label(option).into_owned(),
                    current == option,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.choose(index, option, cx))),
            );
        }

        segmented(buttons).into_any_element()
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
                    choice_label(option).into_owned(),
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
        let current = self.selected_choice(index);
        let unset = self.unset_label(index);
        let shown = if current.is_empty() {
            unset.clone()
        } else {
            choice_label(&current).into_owned()
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
            .gap_0p5()
            .p_1()
            .rounded(theme::radius_panel())
            .border_1()
            .border_color(theme::rule_strong())
            .bg(theme::raised())
            .shadow_lg()
            .on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                if this.open_choice == Some(index) {
                    this.open_choice = None;
                    cx.notify();
                }
            }))
            .children(entries.into_iter().enumerate().map(|(position, option)| {
                let label = if option.is_empty() {
                    unset.clone()
                } else {
                    choice_label(option).into_owned()
                };
                menu_row(
                    ("dropdown-option", index * 100 + position),
                    label,
                    current == option,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.choose(index, option, cx)))
            }));

        div()
            .relative()
            .child(
                select_trigger(("dropdown", index), shown, open)
                    .w(px(DROPDOWN_WIDTH))
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
                        .child(div().text_color(theme::text_faint()).child("="))
                        .child(div().w(px(220.0)).child(pair.value.clone()))
                        .child(
                            button(
                                ("remove-pair", index * 1000 + position),
                                "Remove",
                                ButtonStyle::Danger,
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
            .px_4()
            .py_3()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_start()
                    .gap_4()
                    .child(
                        div()
                            .w(px(LABEL_WIDTH))
                            .flex_none()
                            .pt(px(LABEL_TOP))
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::text())
                            .child(field.label),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .min_w_0()
                            .flex_1()
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .gap_2()
                                    .child(control)
                                    .when(!field.unit.is_empty(), |this| {
                                        this.child(
                                            div()
                                                .text_sm()
                                                .text_color(theme::text_faint())
                                                .child(field.unit),
                                        )
                                    }),
                            )
                            .when_some(error, |this, error| {
                                this.child(div().text_xs().text_color(theme::danger()).child(error))
                            }),
                    ),
            )
            .into_any_element()
    }

    fn render_section_title(
        &self,
        title: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let searching = self.searching();

        div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text())
                    .child(title),
            )
            .when(searching, |this| {
                this.child(
                    button("settings-search-clear", "Clear", ButtonStyle::Quiet, true)
                        .on_click(cx.listener(|this, _, _, cx| this.clear_search(cx))),
                )
            })
            .when(!searching, |this| {
                this.child(
                    button(
                        "settings-section-reset",
                        "Reset to defaults",
                        ButtonStyle::Quiet,
                        !self.section_is_default(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.reset_section(cx))),
                )
            })
    }

    fn render_roles(&self, match_count: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let mut tabs = Vec::with_capacity(ROLES.len());
        let searching = self.searching();
        let title = if searching {
            "Results"
        } else {
            self.section.title()
        };
        let detail = if searching {
            match match_count {
                0 => "Nothing matches this search".to_owned(),
                1 => "1 setting".to_owned(),
                count => format!("{count} settings"),
            }
        } else {
            section_summary(self.section, self.role).to_owned()
        };

        for (position, role) in ROLES.into_iter().enumerate() {
            let unsaved = if role == self.role {
                self.dirty
            } else {
                self.parked.as_ref().is_some_and(|state| state.dirty)
            };

            tabs.push(
                segment(("role", position), role_label(role), role == self.role)
                    .gap_1p5()
                    .when(unsaved, |this| {
                        this.child(
                            div()
                                .size(px(MARK_SIZE))
                                .rounded_full()
                                .bg(theme::warning()),
                        )
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.switch_role(role, cx))),
            );
        }

        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap_4()
            .px_6()
            .py_3()
            .border_b_1()
            .border_color(theme::rule())
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .flex_col()
                    .gap_0p5()
                    .child(self.render_section_title(title, cx))
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme::text_muted())
                            .child(detail),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_faint())
                            .child("Editing"),
                    )
                    .child(segmented(tabs)),
            )
    }

    pub fn note_host_launch(&mut self, enabled: bool) {
        let Some(index) = FIELDS
            .iter()
            .position(|field| field.path == "peers.host_on_launch")
        else {
            return;
        };
        let apply = |saved: &mut Config, form: &mut FormModel| {
            saved.peers.host_on_launch = enabled;
            form.set_flag(index, enabled);
        };

        if self.role == ConfigRole::Host {
            apply(&mut self.saved, &mut self.form);
            self.dirty = self.form.build().is_ok_and(|config| config != self.saved);
        } else if let Some(state) = &mut self.parked {
            apply(&mut state.saved, &mut state.form);
            state.dirty = state.form.build().is_ok_and(|config| config != state.saved);
        }
    }

    fn settle_section(&mut self) {
        if !self.form.visible_fields(self.section).is_empty() {
            return;
        }

        if let Some(section) = Section::ALL
            .into_iter()
            .find(|section| !self.form.visible_fields(*section).is_empty())
        {
            self.section = section;
        }
    }

    fn sync_hardware(&mut self, cx: &mut Context<Self>) {
        let Ok(config) = self.form.build() else {
            return;
        };
        let key = match self.role {
            ConfigRole::Host => DeviceKey::Encoder(config.video.encoder.clone()),
            ConfigRole::Client => DeviceKey::Decoder(config.video.decoder.clone()),
        };
        if self.probe.as_ref().is_some_and(|probe| probe.key == key) {
            return;
        }

        self.probe_generation = self.probe_generation.wrapping_add(1);
        let generation = self.probe_generation;
        self.probe = Some(HardwareProbe {
            key: key.clone(),
            formats: None,
        });
        let pending = cx.background_spawn(async move {
            match key {
                DeviceKey::Encoder(encoder) => {
                    let automatic = matches!(encoder, VideoEncoderConfig::Auto(_));
                    let resolved = rusplay_host::resolve_encoder(&encoder);
                    rusplay_host::encoder_formats(&resolved)
                        .map(|formats| Detected {
                            backend: outlook::detected(
                                outlook::encoder_backend(&resolved),
                                automatic,
                            ),
                            formats,
                        })
                        .map_err(|error| error.to_string())
                }
                DeviceKey::Decoder(decoder) => {
                    let automatic = matches!(decoder, VideoDecoderConfig::Auto(_));
                    let resolved = rusplay_client::resolve_decoder(&decoder);
                    rusplay_client::decoder_formats(&resolved)
                        .map(|formats| Detected {
                            backend: outlook::detected(
                                outlook::decoder_backend(&resolved),
                                automatic,
                            ),
                            formats,
                        })
                        .map_err(|error| error.to_string())
                }
            }
        });
        self.probe_task = Some(cx.spawn(async move |this, cx| {
            let formats = pending.await;
            this.update(cx, |this, cx| {
                if this.probe_generation == generation
                    && let Some(probe) = &mut this.probe
                {
                    probe.formats = Some(formats);
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    fn outlook_lines(&self) -> Vec<OutlookLine> {
        let mut lines = Vec::new();
        if let Some(text) = outlook::audio_gap(
            self.form.text(field_index("audio.codec")).trim(),
            cfg!(feature = "flac"),
        ) {
            lines.push(OutlookLine {
                text: text.to_owned(),
                attention: true,
            });
        }

        let Ok(config) = self.form.build() else {
            return lines;
        };
        let key = match self.role {
            ConfigRole::Host => DeviceKey::Encoder(config.video.encoder.clone()),
            ConfigRole::Client => DeviceKey::Decoder(config.video.decoder.clone()),
        };
        let Some(probe) = &self.probe else {
            return lines;
        };
        if probe.key != key {
            lines.push(checking_line(self.role));
            return lines;
        }

        match &probe.formats {
            None => lines.push(checking_line(self.role)),
            Some(Err(error)) => lines.push(OutlookLine {
                text: error.clone(),
                attention: true,
            }),
            Some(Ok(detected)) => match self.role {
                ConfigRole::Host => lines.push(outlook::encoder_outlook(
                    &detected.backend,
                    rusplay_config::format_preference(
                        config.video.codec,
                        config.video.pixel_format,
                        None,
                    ),
                    detected.formats,
                )),
                ConfigRole::Client => {
                    lines.extend(outlook::decoder_outlook(
                        &detected.backend,
                        detected.formats,
                    ));
                }
            },
        }

        lines
    }

    fn render_outlook(&self) -> AnyElement {
        let lines = self.outlook_lines();
        if lines.is_empty() {
            return div().into_any_element();
        }

        div()
            .flex()
            .flex_none()
            .flex_col()
            .gap_1()
            .px_6()
            .py_2p5()
            .border_t_1()
            .border_color(theme::rule())
            .bg(theme::chrome())
            .children(lines.into_iter().map(|line| {
                div()
                    .text_xs()
                    .map(|this| {
                        if line.attention {
                            this.pl_2()
                                .border_l_2()
                                .border_color(theme::warning())
                                .text_color(theme::text())
                        } else {
                            this.text_color(theme::text_muted())
                        }
                    })
                    .child(line.text)
            }))
            .into_any_element()
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let location = self
            .environment
            .config_path(self.role)
            .map_or_else(|error| error.to_string(), |path| path.display().to_string());

        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap_4()
            .px_6()
            .py_2p5()
            .border_t_1()
            .border_color(theme::rule())
            .bg(theme::chrome())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .when(!self.errors.is_empty(), |this| {
                        let count = self.errors.len();
                        let label = if count == 1 {
                            "1 setting needs attention".to_owned()
                        } else {
                            format!("{count} settings need attention")
                        };

                        this.child(state_text(label, Tone::Danger))
                    })
                    .when(self.dirty && self.errors.is_empty(), |this| {
                        this.child(state_text("Unsaved changes", Tone::Warning))
                    })
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(theme::text_faint())
                            .child(location),
                    ),
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
                        button("settings-save", "Save", ButtonStyle::Primary, self.dirty)
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    ),
            )
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let searching = self.searching();
        let visible = self.listed_fields();
        let match_count = visible.len();
        let anchor = self.scroll_to.take();
        let anchor_field = match anchor {
            Some(ScrollTo::Field(field)) => Some(field),
            Some(ScrollTo::Top) | None => None,
        };
        let mut groups: Vec<(String, Vec<AnyElement>, bool)> = Vec::new();

        for index in visible {
            let field = &FIELDS[index];
            let heading = if searching {
                format!("{} · {}", field.section.title(), field.group)
            } else {
                field.group.to_owned()
            };
            let anchored = anchor_field == Some(index);
            let row = self.render_field(index, cx);

            match groups.last_mut() {
                Some((group, rows, group_anchored)) if group == &heading => {
                    rows.push(divider().into_any_element());
                    rows.push(row);
                    *group_anchored |= anchored;
                }
                _ => groups.push((heading, vec![row], anchored)),
            }
        }

        let mut children = Vec::with_capacity(groups.len() * 2);
        let mut anchor_child = None;

        for (heading, rows, anchored) in groups {
            if anchored && anchor_child.is_none() {
                anchor_child = Some(children.len());
            }

            children.push(group_heading(heading).into_any_element());
            children.push(panel().p_0().py_1().children(rows).into_any_element());
        }

        self.apply_scroll(anchor.as_ref(), anchor_child);

        let mut fields = div()
            .id("settings-fields")
            .track_scroll(&self.scroll)
            .flex()
            .flex_col()
            .flex_1()
            .overflow_y_scroll()
            .px_6()
            .pb_8();
        fields = if children.is_empty() {
            fields.child(empty_results(searching))
        } else {
            fields.children(children)
        };
        let sidebar = self.render_sidebar(cx);
        let outlook = self.render_outlook();
        let footer = self.render_footer(cx);
        let roles = self.render_roles(match_count, cx);

        div().flex().size_full().child(sidebar).child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(roles)
                .child(fields)
                .child(outlook)
                .child(footer),
        )
    }
}

impl SettingsView {
    fn apply_scroll(&self, anchor: Option<&ScrollTo>, child: Option<usize>) {
        match anchor {
            Some(ScrollTo::Top) => self.scroll.set_offset(point(px(0.0), px(0.0))),
            Some(ScrollTo::Field(_)) => {
                if let Some(child) = child {
                    self.scroll.scroll_to_top_of_item(child);
                }
            }
            None => {}
        }
    }
}

fn field_index(path: &str) -> usize {
    FIELDS
        .iter()
        .position(|field| field.path == path)
        .unwrap_or(0)
}

fn checking_line(role: ConfigRole) -> OutlookLine {
    OutlookLine {
        text: match role {
            ConfigRole::Host => "Checking what this encoder can send…".to_owned(),
            ConfigRole::Client => "Checking what this decoder can play…".to_owned(),
        },
        attention: false,
    }
}

fn divider() -> impl IntoElement {
    div().mx_4().h(px(1.0)).bg(theme::rule())
}

fn empty_results(searching: bool) -> impl IntoElement {
    let text = if searching {
        "No setting matches. Try the name from the config file, or clear the search."
    } else {
        "Nothing to show here: the current choices hide every option in this section."
    };

    div()
        .pt_6()
        .text_sm()
        .text_color(theme::text_muted())
        .child(text)
}
