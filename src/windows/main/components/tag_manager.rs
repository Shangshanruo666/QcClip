use crate::ui::scale::ui_rem;

use std::collections::{HashMap, HashSet};

use chrono::{Local, TimeZone};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, ElementId, Entity, FontWeight, Hsla, KeyDownEvent, MouseButton,
    PromptLevel, Subscription, Window, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};

use crate::domain::clipboard::Tag;
use crate::i18n;
use crate::services::database::{
    TagCreateError, TagDeleteError, TagQueryError, TagRepository, TagUpdateError,
};
use crate::ui::{hover_icon, icon};
use crate::windows::dialog::{
    DialogMode, DialogWindowPosition, dialog_body, dialog_button, dialog_footer, dialog_header,
    open_adaptive_window,
};
use crate::windows::main::state::MainWindow;

const SELECT_COLUMN_WIDTH: f32 = 42.0;
const COLOR_COLUMN_WIDTH: f32 = 124.0;
const CREATED_COLUMN_WIDTH: f32 = 142.0;
const UPDATED_COLUMN_WIDTH: f32 = 142.0;
const ACTION_COLUMN_WIDTH: f32 = 68.0;
const MANAGER_WIDTH: f32 = 760.0;
const TABLE_MAX_HEIGHT: f32 = 400.0;

pub(crate) fn open(
    repository: TagRepository,
    main_window: Entity<MainWindow>,
    window: &mut Window,
    cx: &mut Context<MainWindow>,
) {
    let result = open_adaptive_window(
        Some(window),
        cx,
        DialogWindowPosition::Center,
        DialogMode::Modal,
        move |window, cx| cx.new(|cx| TagManager::new(repository, main_window, window, cx)),
    );
    if let Err(error) = result {
        tracing::error!("打开标签管理窗口失败：{error}");
    }
}

struct NewTagDraft {
    name_input: Entity<InputState>,
    color: u32,
}

pub(crate) struct TagManager {
    repository: TagRepository,
    main_window: Entity<MainWindow>,
    tags: Vec<Tag>,
    selected_ids: HashSet<i64>,
    hovered_delete_id: Option<i64>,
    editing_name_id: Option<i64>,
    name_inputs: HashMap<i64, Entity<InputState>>,
    name_subscriptions: HashMap<i64, Subscription>,
    saving_tag_ids: HashSet<i64>,
    new_tag: Option<NewTagDraft>,
    new_tag_subscription: Option<Subscription>,
    new_tag_saving: bool,
    deleting: bool,
    loading: bool,
    load_generation: u64,
    error_message: Option<String>,
    search_query: String,
    search_input: Entity<InputState>,
    _search_input_subscription: Subscription,
}

impl TagManager {
    fn new(
        repository: TagRepository,
        main_window: Entity<MainWindow>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let translator = i18n::translator(cx);
        let search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(translator.translate("main.tags.manager.search_placeholder"))
        });
        let search_input_subscription =
            cx.subscribe_in(&search_input, window, |this, state, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.search_query = state.read(cx).value().to_string();
                    cx.notify();
                }
            });
        let mut manager = Self {
            repository,
            main_window,
            tags: Vec::new(),
            selected_ids: HashSet::new(),
            hovered_delete_id: None,
            editing_name_id: None,
            name_inputs: HashMap::new(),
            name_subscriptions: HashMap::new(),
            saving_tag_ids: HashSet::new(),
            new_tag: None,
            new_tag_subscription: None,
            new_tag_saving: false,
            deleting: false,
            loading: false,
            load_generation: 0,
            error_message: None,
            search_query: String::new(),
            search_input,
            _search_input_subscription: search_input_subscription,
        };
        manager.load_tags(cx);
        manager
    }

    fn load_tags(&mut self, cx: &mut Context<Self>) {
        self.load_generation = self.load_generation.wrapping_add(1);
        let generation = self.load_generation;
        self.loading = true;
        self.error_message = None;
        let repository = self.repository.clone();
        let task = cx.background_spawn(async move { repository.list() });
        cx.spawn(async move |this, cx| match task.await {
            Ok(tags) => {
                this.update(cx, |this, cx| {
                    if generation != this.load_generation {
                        return;
                    }
                    this.tags = tags;
                    this.tags.sort_by(|left, right| {
                        right
                            .sort_order
                            .cmp(&left.sort_order)
                            .then_with(|| right.id.cmp(&left.id))
                    });
                    this.loading = false;
                    cx.notify();
                })
                .ok();
            }
            Err(error) => {
                this.update(cx, |this, cx| {
                    if generation != this.load_generation {
                        return;
                    }
                    this.loading = false;
                    this.error_message = Some(load_error_message(&error, cx));
                    tracing::error!("标签管理窗口加载失败：{error}");
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }

    fn toggle_selected(&mut self, tag_id: i64, selected: bool, cx: &mut Context<Self>) {
        if selected {
            self.selected_ids.insert(tag_id);
        } else {
            self.selected_ids.remove(&tag_id);
        }
        cx.notify();
    }

    fn set_all_selected(&mut self, selected: bool, cx: &mut Context<Self>) {
        if selected {
            self.selected_ids = self.tags.iter().map(|tag| tag.id).collect();
        } else {
            self.selected_ids.clear();
        }
        cx.notify();
    }

    fn begin_edit_name(&mut self, tag_id: i64, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing_name_id == Some(tag_id) {
            return;
        }
        self.cancel_name_edit(cx);
        let Some(tag) = self.tags.iter().find(|tag| tag.id == tag_id) else {
            return;
        };
        let translator = i18n::translator(cx);
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(translator.translate("main.tags.manager.name_placeholder"))
                .default_value(tag.name.clone())
        });
        let subscription =
            cx.subscribe_in(
                &input,
                window,
                move |this, state, event, _, cx| match event {
                    InputEvent::PressEnter { .. } => {
                        let name = state.read(cx).value().to_string();
                        this.save_name(tag_id, name, cx);
                    }
                    InputEvent::Blur => this.cancel_name_edit(cx),
                    _ => {}
                },
            );
        self.name_inputs.insert(tag_id, input.clone());
        self.name_subscriptions.insert(tag_id, subscription);
        self.editing_name_id = Some(tag_id);
        let input_for_focus = input.clone();
        window.defer(cx, move |window, cx| {
            input_for_focus.update(cx, |input, cx| input.focus(window, cx));
        });
        cx.notify();
    }

    fn cancel_name_edit(&mut self, cx: &mut Context<Self>) {
        let Some(tag_id) = self.editing_name_id.take() else {
            return;
        };
        self.name_inputs.remove(&tag_id);
        self.name_subscriptions.remove(&tag_id);
        cx.notify();
    }

    fn save_name(&mut self, tag_id: i64, name: String, cx: &mut Context<Self>) {
        if self.saving_tag_ids.contains(&tag_id) {
            return;
        }
        let Some(tag) = self.tags.iter().find(|tag| tag.id == tag_id) else {
            return;
        };
        let name = name.trim().to_owned();
        if name.is_empty() {
            self.error_message =
                Some(i18n::translator(cx).translate("main.form.tag.error.empty_name"));
            cx.notify();
            return;
        }
        let color = tag.color.clone();
        let repository = self.repository.clone();
        self.saving_tag_ids.insert(tag_id);
        self.error_message = None;
        let task = cx.background_spawn(async move { repository.update(tag_id, &name, &color) });
        cx.spawn(async move |this, cx| match task.await {
            Ok(tag) => {
                this.update(cx, |this, cx| this.finish_tag_update(tag, true, cx))
                    .ok();
            }
            Err(error) => {
                if matches!(
                    &error,
                    TagUpdateError::NotFound(_)
                        | TagUpdateError::Database(_)
                        | TagUpdateError::Sql(_)
                ) {
                    tracing::error!(tag_id, %error, "修改标签名称失败");
                }
                this.update(cx, |this, cx| {
                    this.saving_tag_ids.remove(&tag_id);
                    this.error_message = Some(update_error_message(&error, cx));
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }

    fn open_tag_color_picker(&self, tag_id: i64, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving_tag_ids.contains(&tag_id) {
            return;
        }
        let Some(tag) = self.tags.iter().find(|tag| tag.id == tag_id) else {
            return;
        };
        let manager = cx.entity();
        let result = crate::windows::color_picker::open_color_picker_window(
            Some(window),
            parse_hex_color(&tag.color).unwrap_or(0x1677ff),
            move |color, cx| {
                manager.update(cx, |manager, cx| manager.save_color(tag_id, color, cx));
            },
            cx,
        );
        if let Err(error) = result {
            tracing::error!("打开标签颜色窗口失败：{error}");
        }
    }

    fn save_color(&mut self, tag_id: i64, color: u32, cx: &mut Context<Self>) {
        if self.saving_tag_ids.contains(&tag_id) {
            return;
        }
        let Some(tag) = self.tags.iter().find(|tag| tag.id == tag_id) else {
            return;
        };
        let name = tag.name.clone();
        let color = format!("#{color:06X}");
        let repository = self.repository.clone();
        self.saving_tag_ids.insert(tag_id);
        self.error_message = None;
        let task = cx.background_spawn(async move { repository.update(tag_id, &name, &color) });
        cx.spawn(async move |this, cx| match task.await {
            Ok(tag) => {
                this.update(cx, |this, cx| this.finish_tag_update(tag, false, cx))
                    .ok();
            }
            Err(error) => {
                if matches!(
                    &error,
                    TagUpdateError::NotFound(_)
                        | TagUpdateError::Database(_)
                        | TagUpdateError::Sql(_)
                ) {
                    tracing::error!(tag_id, %error, "修改标签颜色失败");
                }
                this.update(cx, |this, cx| {
                    this.saving_tag_ids.remove(&tag_id);
                    this.error_message = Some(update_error_message(&error, cx));
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }

    fn finish_tag_update(&mut self, tag: Tag, clear_name_edit: bool, cx: &mut Context<Self>) {
        let tag_id = tag.id;
        self.saving_tag_ids.remove(&tag_id);
        self.tags.retain(|existing| existing.id != tag_id);
        self.tags.push(tag.clone());
        self.tags.sort_by(|left, right| {
            right
                .sort_order
                .cmp(&left.sort_order)
                .then_with(|| right.id.cmp(&left.id))
        });
        if clear_name_edit && self.editing_name_id == Some(tag_id) {
            self.editing_name_id = None;
            self.name_inputs.remove(&tag_id);
            self.name_subscriptions.remove(&tag_id);
        }
        self.main_window
            .update(cx, |main, cx| main.save_tag(tag, cx));
        cx.notify();
    }

    fn add_new_tag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(draft) = &self.new_tag {
            let input = draft.name_input.clone();
            window.defer(cx, move |window, cx| {
                input.update(cx, |input, cx| input.focus(window, cx));
            });
            return;
        }
        let translator = i18n::translator(cx);
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(translator.translate("main.tags.manager.name_placeholder"))
        });
        let subscription =
            cx.subscribe_in(&input, window, |this, state, event, _, cx| match event {
                InputEvent::PressEnter { .. } => {
                    let name = state.read(cx).value().to_string();
                    this.save_new_tag(name, cx);
                }
                InputEvent::Blur => {}
                _ => {}
            });
        self.new_tag = Some(NewTagDraft {
            name_input: input.clone(),
            color: hsla_to_rgb(cx.theme().colors.blue),
        });
        self.new_tag_subscription = Some(subscription);
        let input_for_focus = input.clone();
        window.defer(cx, move |window, cx| {
            input_for_focus.update(cx, |input, cx| input.focus(window, cx));
        });
        cx.notify();
    }

    fn cancel_new_tag(&mut self, cx: &mut Context<Self>) {
        if self.new_tag_saving {
            return;
        }
        self.new_tag = None;
        self.new_tag_subscription = None;
        self.error_message = None;
        cx.notify();
    }

    fn submit_or_cancel_new_tag(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.new_tag.as_ref().map(|draft| draft.name_input.clone()) else {
            return;
        };
        let name = input.read(cx).value().to_string();
        if name.trim().is_empty() {
            self.cancel_new_tag(cx);
        } else {
            self.save_new_tag(name, cx);
        }
    }

    fn open_new_color_picker(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = &self.new_tag else {
            return;
        };
        if self.new_tag_saving {
            return;
        }
        let manager = cx.entity();
        let initial_color = draft.color;
        let result = crate::windows::color_picker::open_color_picker_window(
            Some(window),
            initial_color,
            move |color, cx| {
                manager.update(cx, |manager, cx| manager.set_new_color(color, cx));
            },
            cx,
        );
        if let Err(error) = result {
            tracing::error!("打开新标签颜色窗口失败：{error}");
        }
    }

    fn set_new_color(&mut self, color: u32, cx: &mut Context<Self>) {
        if let Some(draft) = &mut self.new_tag {
            draft.color = color;
            cx.notify();
        }
    }

    fn save_new_tag(&mut self, name: String, cx: &mut Context<Self>) {
        if self.new_tag_saving {
            return;
        }
        let name = name.trim().to_owned();
        if name.is_empty() {
            self.error_message =
                Some(i18n::translator(cx).translate("main.form.tag.error.empty_name"));
            cx.notify();
            return;
        }
        let Some(draft) = &self.new_tag else {
            return;
        };
        let color = format!("#{:06X}", draft.color);
        let repository = self.repository.clone();
        self.new_tag_saving = true;
        self.error_message = None;
        let task = cx.background_spawn(async move { repository.create(&name, &color) });
        cx.spawn(async move |this, cx| match task.await {
            Ok(tag) => {
                this.update(cx, |this, cx| this.finish_new_tag(tag, cx))
                    .ok();
            }
            Err(error) => {
                if matches!(
                    &error,
                    TagCreateError::Database(_)
                        | TagCreateError::Sql(_)
                        | TagCreateError::SortOrderOverflow
                ) {
                    tracing::error!(%error, "创建标签失败");
                }
                this.update(cx, |this, cx| {
                    this.new_tag_saving = false;
                    this.error_message = Some(create_error_message(&error, cx));
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }

    fn finish_new_tag(&mut self, tag: Tag, cx: &mut Context<Self>) {
        self.tags.push(tag.clone());
        self.tags.sort_by(|left, right| {
            right
                .sort_order
                .cmp(&left.sort_order)
                .then_with(|| right.id.cmp(&left.id))
        });
        self.new_tag = None;
        self.new_tag_subscription = None;
        self.new_tag_saving = false;
        self.main_window
            .update(cx, |main, cx| main.save_tag(tag, cx));
        cx.notify();
    }

    fn confirm_delete(&mut self, tag_ids: Vec<i64>, window: &mut Window, cx: &mut Context<Self>) {
        if tag_ids.is_empty()
            || self.deleting
            || tag_ids
                .iter()
                .any(|tag_id| self.saving_tag_ids.contains(tag_id))
        {
            return;
        }
        let translator = i18n::translator(cx);
        let (message, detail) = if tag_ids.len() == 1 {
            let tag_name = self
                .tags
                .iter()
                .find(|tag| tag.id == tag_ids[0])
                .map(|tag| tag.name.clone())
                .unwrap_or_default();
            (
                translator
                    .translate_with_args("main.confirm.delete_tag.title", &[("name", tag_name)]),
                translator.translate("main.confirm.delete_tag.detail"),
            )
        } else {
            (
                translator.translate_with_args(
                    "main.confirm.delete_tags.title",
                    &[("count", tag_ids.len().to_string())],
                ),
                translator.translate("main.confirm.delete_tags.detail"),
            )
        };
        let delete = gpui::PromptButton::new(translator.translate("main.menu.delete"));
        let cancel = gpui::PromptButton::cancel(translator.translate("main.form.cancel"));
        let answer = window.prompt(
            PromptLevel::Warning,
            &message,
            Some(&detail),
            &[delete, cancel],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await.unwrap_or(1) == 0 {
                this.update(cx, |this, cx| this.delete_tags(tag_ids, cx))
                    .ok();
            }
        })
        .detach();
    }

    fn delete_tags(&mut self, tag_ids: Vec<i64>, cx: &mut Context<Self>) {
        if tag_ids.is_empty() || self.deleting {
            return;
        }
        if !crate::windows::main::MainWindowService::begin_blocking_operation(
            "main.menu.delete",
            cx,
        ) {
            return;
        }
        self.deleting = true;
        self.error_message = None;
        let repository = self.repository.clone();
        let delete_ids = tag_ids.clone();
        let task = cx.background_spawn(async move { repository.delete_many(&delete_ids) });
        cx.spawn(async move |this, cx| {
            match task.await {
                Ok(_) => {
                    this.update(cx, |this, cx| {
                        this.tags.retain(|tag| !tag_ids.contains(&tag.id));
                        this.selected_ids.retain(|tag_id| !tag_ids.contains(tag_id));
                        this.deleting = false;
                        this.main_window
                            .update(cx, |main, cx| main.remove_tags(&tag_ids, cx));
                        cx.notify();
                    })
                    .ok();
                }
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.deleting = false;
                        this.error_message = Some(delete_error_message(&error, cx));
                        tracing::error!("批量删除标签失败：{error}");
                        cx.notify();
                    })
                    .ok();
                }
            }
            cx.update(crate::windows::main::MainWindowService::finish_blocking_operation);
        })
        .detach();
        cx.notify();
    }

    fn render_search_input(&self, theme: gpui_component::ThemeColor) -> AnyElement {
        let has_search_text = !self.search_query.is_empty();
        let clear_button = if has_search_text {
            let search_input = self.search_input.clone();
            Some(
                div()
                    .id("tag-manager-search-clear-button")
                    .group("tag-manager-search-clear-button")
                    .size(ui_rem(20.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|this| this.bg(theme.secondary_hover))
                    .on_click(move |_, window, cx| {
                        search_input.update(cx, |input, cx| {
                            input.set_value("", window, cx);
                            input.focus(window, cx);
                        });
                    })
                    .child(hover_icon(
                        "tag-manager-search-clear-button",
                        "x",
                        14.0,
                        theme.danger,
                    )),
            )
        } else {
            None
        };
        let mut search_input = Input::new(&self.search_input)
            .small()
            .cleanable(false)
            .w_full();
        if let Some(clear_button) = clear_button {
            search_input = search_input.suffix(clear_button);
        }
        div()
            .flex_1()
            .min_w_0()
            .max_w(ui_rem(280.0))
            .mx(ui_rem(12.0))
            .child(search_input)
            .into_any_element()
    }

    fn render_toolbar(
        &self,
        theme: gpui_component::ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let translator = i18n::translator(cx);
        let selected_count = self.selected_ids.len();
        let manager = cx.entity();
        let add_button = Button::new("tag-manager-add")
            .label(translator.translate("main.tags.manager.add"))
            .icon(icon("plus", 13.0, theme.foreground))
            .small()
            .disabled(self.loading || self.new_tag.is_some())
            .on_click(move |_, window, cx| {
                manager.update(cx, |manager, cx| manager.add_new_tag(window, cx));
            });
        let manager = cx.entity();
        let delete_disabled = self.loading || self.deleting || selected_count == 0;
        let delete_icon_color = if delete_disabled {
            gpui::ColorExt::opacity(&theme.muted_foreground, 0.5)
        } else {
            theme.danger_foreground
        };
        let delete_button = Button::new("tag-manager-delete-selected")
            .label(translator.translate("main.tags.manager.delete_selected"))
            .icon(icon("trash", 13.0, delete_icon_color))
            .small()
            .danger()
            .disabled(delete_disabled)
            .on_click(move |_, window, cx| {
                let ids = manager
                    .read(cx)
                    .selected_ids
                    .iter()
                    .copied()
                    .collect::<Vec<_>>();
                manager.update(cx, |manager, cx| manager.confirm_delete(ids, window, cx));
            });
        h_flex()
            .w_full()
            .justify_between()
            .child(add_button)
            .child(self.render_search_input(theme))
            .child(
                h_flex()
                    .gap(ui_rem(8.0))
                    .child(
                        div()
                            .text_size(ui_rem(11.0))
                            .text_color(theme.muted_foreground)
                            .child(translator.translate_with_args(
                                "main.tags.manager.selected_count",
                                &[("count", selected_count.to_string())],
                            )),
                    )
                    .child(delete_button),
            )
            .into_any_element()
    }

    fn render_table(
        &self,
        theme: gpui_component::ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let translator = i18n::translator(cx);
        let all_selected = !self.tags.is_empty() && self.selected_ids.len() == self.tags.len();
        let search_query = self.search_query.trim().to_lowercase();
        let visible_tags = self
            .tags
            .iter()
            .filter(|tag| tag.name.to_lowercase().contains(&search_query))
            .collect::<Vec<_>>();
        let manager = cx.entity();
        let header = div()
            .w_full()
            .h(ui_rem(36.0))
            .flex()
            .items_center()
            .flex_none()
            .bg(theme.table_head)
            .border_b(px(0.5))
            .border_color(theme.table_row_border)
            .child(
                div()
                    .w(px(SELECT_COLUMN_WIDTH))
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Checkbox::new("tag-manager-select-all")
                            .large()
                            .checked(all_selected)
                            .disabled(self.loading || self.tags.is_empty() || self.deleting)
                            .on_click(move |checked, _, cx| {
                                manager.update(cx, |manager, cx| {
                                    manager.set_all_selected(*checked, cx)
                                });
                            }),
                    ),
            )
            .child(table_header_cell(
                translator.translate("main.tags.manager.name"),
                None,
                theme,
            ))
            .child(table_header_cell(
                translator.translate("main.tags.manager.color"),
                Some(COLOR_COLUMN_WIDTH),
                theme,
            ))
            .child(table_header_cell(
                translator.translate("main.tags.manager.created_at"),
                Some(CREATED_COLUMN_WIDTH),
                theme,
            ))
            .child(table_header_cell(
                translator.translate("main.tags.manager.updated_at"),
                Some(UPDATED_COLUMN_WIDTH),
                theme,
            ))
            .child(table_header_cell(
                translator.translate("main.tags.manager.operation"),
                Some(ACTION_COLUMN_WIDTH),
                theme,
            ));

        let mut rows = Vec::with_capacity(visible_tags.len() + usize::from(self.new_tag.is_some()));
        if self.new_tag.is_some() {
            rows.push(self.render_new_tag_row(theme, cx));
        }
        for (index, tag) in visible_tags.iter().enumerate() {
            rows.push(self.render_tag_row(tag, index, theme, cx));
        }
        let body = if self.loading {
            manager_status(translator.translate("main.tags.manager.loading"), theme)
        } else if self.tags.is_empty() && self.new_tag.is_none() {
            manager_status(translator.translate("main.tags.manager.empty"), theme)
        } else if visible_tags.is_empty() && self.new_tag.is_none() {
            manager_status(
                translator.translate("main.tags.manager.search_empty"),
                theme,
            )
        } else if rows.is_empty() {
            manager_status(translator.translate("main.tags.manager.empty"), theme)
        } else {
            v_flex()
                .flex_1()
                .overflow_hidden()
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .child(v_flex().size_full().overflow_y_scrollbar().children(rows)),
                )
                .into_any_element()
        };
        div()
            .w_full()
            .max_h(px(TABLE_MAX_HEIGHT + 36.0))
            .flex()
            .flex_col()
            .border_1()
            .border_color(theme.border)
            .rounded_md()
            .overflow_hidden()
            .child(header)
            .child(body)
            .into_any_element()
    }

    fn render_tag_row(
        &self,
        tag: &Tag,
        index: usize,
        theme: gpui_component::ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tag_id = tag.id;
        let selected = self.selected_ids.contains(&tag_id);
        let delete_hovered = self.hovered_delete_id == Some(tag_id);
        let delete_icon_color = if delete_hovered {
            theme.danger_foreground
        } else {
            theme.danger
        };
        let manager = cx.entity();
        let row_background = if index % 2 == 1 {
            theme.table_even
        } else {
            theme.background
        };
        let color = parse_hex_color(&tag.color).unwrap_or(0x1677ff);
        let mut row = div()
            .id(("tag-manager-row", tag_id as u64))
            .w_full()
            .h(ui_rem(46.0))
            .flex()
            .items_center()
            .flex_none()
            .bg(row_background)
            .border_b(px(0.5))
            .border_color(theme.table_row_border)
            .child(
                div()
                    .w(px(SELECT_COLUMN_WIDTH))
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Checkbox::new(("tag-manager-select", tag_id as u64))
                            .large()
                            .checked(selected)
                            .disabled(self.deleting)
                            .on_click(move |checked, _, cx| {
                                manager.update(cx, |manager, cx| {
                                    manager.toggle_selected(tag_id, *checked, cx)
                                });
                            }),
                    ),
            )
            .child(self.render_name_cell(tag, theme, cx))
            .child(self.render_color_cell(tag, color, theme, cx))
            .child(table_date_cell(
                format_timestamp(tag.created_at),
                CREATED_COLUMN_WIDTH,
                theme,
            ))
            .child(table_date_cell(
                format_timestamp(tag.updated_at),
                UPDATED_COLUMN_WIDTH,
                theme,
            ));

        let manager = cx.entity();
        row = row.child(
            div()
                .w(px(ACTION_COLUMN_WIDTH))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .id(("tag-manager-delete", tag_id as u64))
                        .group(format!("tag-manager-delete-{tag_id}"))
                        .size(ui_rem(28.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_md()
                        .cursor_pointer()
                        .text_color(delete_icon_color)
                        .hover(|this| {
                            this.bg(theme.danger_hover)
                                .text_color(theme.danger_foreground)
                        })
                        .active(|this| {
                            this.bg(theme.danger_active)
                                .text_color(theme.danger_foreground)
                        })
                        .on_hover(cx.listener(move |manager, hovered, _, cx| {
                            if *hovered {
                                if manager.hovered_delete_id != Some(tag_id) {
                                    manager.hovered_delete_id = Some(tag_id);
                                    cx.notify();
                                }
                            } else if manager.hovered_delete_id == Some(tag_id) {
                                manager.hovered_delete_id = None;
                                cx.notify();
                            }
                        }))
                        .on_click(move |_, window, cx| {
                            manager.update(cx, |manager, cx| {
                                manager.confirm_delete(vec![tag_id], window, cx)
                            });
                        })
                        .child(hover_icon(
                            format!("tag-manager-delete-{tag_id}"),
                            "trash",
                            14.0,
                            delete_icon_color,
                        )),
                ),
        );
        row.into_any_element()
    }

    fn render_name_cell(
        &self,
        tag: &Tag,
        theme: gpui_component::ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tag_id = tag.id;
        if self.editing_name_id == Some(tag_id) {
            if let Some(input) = self.name_inputs.get(&tag_id) {
                let input = input.clone();
                let saving = self.saving_tag_ids.contains(&tag_id);
                let manager = cx.entity();
                let manager_for_cancel = manager.clone();
                return div()
                    .w_0()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .gap(ui_rem(4.0))
                    .px_3()
                    .on_key_down(cx.listener(move |manager, event: &KeyDownEvent, _, cx| {
                        if event.keystroke.key == "escape" {
                            manager.cancel_name_edit(cx);
                            cx.stop_propagation();
                        }
                    }))
                    .child(
                        div()
                            .w_0()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .flex()
                            .items_center()
                            .child(Input::new(&input).w_full().disabled(saving)),
                    )
                    .child(
                        inline_action(
                            ("tag-manager-save-name", tag_id as u64),
                            "check",
                            theme.primary,
                            move |_, _, cx| {
                                manager.update(cx, |manager, cx| {
                                    let name = manager
                                        .name_inputs
                                        .get(&tag_id)
                                        .map(|input| input.read(cx).value().to_string())
                                        .unwrap_or_default();
                                    manager.save_name(tag_id, name, cx);
                                });
                            },
                        )
                        .into_any_element(),
                    )
                    .child(
                        inline_action(
                            ("tag-manager-cancel-name", tag_id as u64),
                            "x",
                            theme.muted_foreground,
                            move |_, _, cx| {
                                manager_for_cancel
                                    .update(cx, |manager, cx| manager.cancel_name_edit(cx));
                            },
                        )
                        .into_any_element(),
                    )
                    .into_any_element();
            }
        }

        let manager = cx.entity();
        div()
            .id(("tag-manager-name", tag_id as u64))
            .w_0()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .items_center()
            .px_3()
            .cursor_pointer()
            .hover(|this| this.bg(theme.table_hover))
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.foreground)
                    .text_size(ui_rem(12.0))
                    .font_weight(FontWeight::MEDIUM)
                    .child(tag.name.clone()),
            )
            .on_click(move |_, window, cx| {
                manager.update(cx, |manager, cx| {
                    manager.begin_edit_name(tag_id, window, cx)
                });
            })
            .into_any_element()
    }

    fn render_color_cell(
        &self,
        tag: &Tag,
        color: u32,
        theme: gpui_component::ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tag_id = tag.id;
        let manager = cx.entity();
        let saving = self.saving_tag_ids.contains(&tag_id);
        div()
            .id(("tag-manager-color", tag_id as u64))
            .w(px(COLOR_COLUMN_WIDTH))
            .h_full()
            .flex()
            .items_center()
            .gap(ui_rem(7.0))
            .px_3()
            .cursor_pointer()
            .when(saving, |this| this.opacity(0.55))
            .hover(|this| this.bg(theme.table_hover))
            .on_click(move |_, window, cx| {
                manager.update(cx, |manager, cx| {
                    manager.open_tag_color_picker(tag_id, window, cx)
                });
            })
            .child(div().size(ui_rem(16.0)).rounded_full().bg(rgb(color)))
            .child(
                div()
                    .text_size(ui_rem(11.0))
                    .text_color(theme.muted_foreground)
                    .child(format!("#{color:06X}")),
            )
            .into_any_element()
    }

    fn render_new_tag_row(
        &self,
        theme: gpui_component::ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(draft) = &self.new_tag else {
            return div().into_any_element();
        };
        let input = draft.name_input.clone();
        let color = draft.color;
        let manager_for_outside = cx.entity();
        let mut row = div()
            .id("tag-manager-new-row")
            .w_full()
            .h(ui_rem(52.0))
            .flex()
            .items_center()
            .flex_none()
            .bg(gpui::ColorExt::opacity(&theme.primary, 0.06))
            .border_b(px(0.5))
            .border_color(theme.table_row_border)
            .on_mouse_down_out(move |_, _, cx| {
                manager_for_outside.update(cx, |manager, cx| {
                    manager.submit_or_cancel_new_tag(cx);
                });
            })
            .child(div().w(px(SELECT_COLUMN_WIDTH)).h_full());

        let saving = self.new_tag_saving;
        let manager_for_save = cx.entity();
        let manager_for_cancel = cx.entity();
        row = row.child(
            div()
                .w_0()
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .items_center()
                .gap(ui_rem(4.0))
                .px_3()
                .on_key_down(cx.listener(move |manager, event: &KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        manager.cancel_new_tag(cx);
                        cx.stop_propagation();
                    }
                }))
                .child(
                    div()
                        .w_0()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .flex()
                        .items_center()
                        .child(Input::new(&input).w_full().disabled(saving)),
                )
                .child(
                    inline_action(
                        "tag-manager-save-new",
                        "check",
                        theme.primary,
                        move |_, _, cx| {
                            manager_for_save.update(cx, |manager, cx| {
                                let name = manager
                                    .new_tag
                                    .as_ref()
                                    .map(|draft| draft.name_input.read(cx).value().to_string())
                                    .unwrap_or_default();
                                manager.save_new_tag(name, cx);
                            });
                        },
                    )
                    .into_any_element(),
                )
                .child(
                    inline_action(
                        "tag-manager-cancel-new",
                        "x",
                        theme.muted_foreground,
                        move |_, _, cx| {
                            manager_for_cancel.update(cx, |manager, cx| manager.cancel_new_tag(cx));
                        },
                    )
                    .into_any_element(),
                ),
        );
        let manager_for_color = cx.entity();
        row = row.child(
            div()
                .id("tag-manager-new-color")
                .w(px(COLOR_COLUMN_WIDTH))
                .h_full()
                .flex()
                .items_center()
                .gap(ui_rem(7.0))
                .px_3()
                .cursor_pointer()
                .hover(|this| this.bg(theme.table_hover))
                .on_click(move |_, window, cx| {
                    manager_for_color
                        .update(cx, |manager, cx| manager.open_new_color_picker(window, cx));
                })
                .child(div().size(ui_rem(16.0)).rounded_full().bg(rgb(color)))
                .child(
                    div()
                        .text_size(ui_rem(11.0))
                        .text_color(theme.muted_foreground)
                        .child(format!("#{color:06X}")),
                ),
        );
        row = row
            .child(table_date_cell("—".to_owned(), CREATED_COLUMN_WIDTH, theme))
            .child(table_date_cell("—".to_owned(), UPDATED_COLUMN_WIDTH, theme))
            .child(div().w(px(ACTION_COLUMN_WIDTH)).h_full());
        row.into_any_element()
    }
}

impl Render for TagManager {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let translator = i18n::translator(cx);
        let focus_handle = cx.focus_handle();
        let error_message = self.error_message.clone();
        let close = dialog_button(
            "tag-manager-close",
            translator.translate("main.tags.manager.close"),
        )
        .on_click(|_, window: &mut Window, _| window.remove_window());

        div()
            .id("tag-manager")
            .w(px(MANAGER_WIDTH))
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.foreground)
            .track_focus(&focus_handle)
            .child(dialog_header(
                translator.translate("main.tags.manager.title"),
                theme,
            ))
            .child(dialog_body(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui_rem(10.0))
                    .child(self.render_toolbar(theme, cx))
                    .when_some(error_message, |this, message| {
                        this.child(
                            div()
                                .text_size(ui_rem(11.0))
                                .text_color(theme.danger)
                                .child(message),
                        )
                    })
                    .child(self.render_table(theme, cx))
                    .child(dialog_footer(vec![close.into_any_element()], theme)),
            ))
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
    }
}

fn inline_action(
    id: impl Into<ElementId>,
    icon_name: &'static str,
    color: Hsla,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let id = id.into();
    let group = format!("tag-manager-inline-{id:?}");
    div()
        .id(id)
        .group(group.clone())
        .size(ui_rem(24.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .cursor_pointer()
        .text_color(color)
        .hover(|this| this.bg(gpui::ColorExt::opacity(&color, 0.12)))
        .on_click(on_click)
        .child(hover_icon(group, icon_name, 13.0, color))
}

fn table_header_cell(
    label: String,
    width: Option<f32>,
    theme: gpui_component::ThemeColor,
) -> impl IntoElement {
    let cell = div()
        .h_full()
        .flex()
        .items_center()
        .px_3()
        .text_size(ui_rem(11.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.table_head_foreground)
        .child(label);
    if let Some(width) = width {
        cell.w(px(width)).flex_none()
    } else {
        cell.w_0().flex_1().min_w_0()
    }
}

fn table_date_cell(
    value: String,
    width: f32,
    theme: gpui_component::ThemeColor,
) -> impl IntoElement {
    div()
        .w(px(width))
        .h_full()
        .flex()
        .items_center()
        .px_3()
        .flex_none()
        .text_size(ui_rem(11.0))
        .text_color(theme.muted_foreground)
        .child(value)
}

fn manager_status(message: String, theme: gpui_component::ThemeColor) -> AnyElement {
    div()
        .w_full()
        .h(ui_rem(76.0))
        .flex()
        .items_center()
        .justify_center()
        .text_size(ui_rem(11.0))
        .text_color(theme.muted_foreground)
        .child(message)
        .into_any_element()
}

fn load_error_message(error: &TagQueryError, cx: &Context<TagManager>) -> String {
    let _ = error;
    i18n::translator(cx).translate("main.tags.manager.load_error")
}

fn delete_error_message(error: &TagDeleteError, cx: &Context<TagManager>) -> String {
    let _ = error;
    i18n::translator(cx).translate("main.tags.manager.delete_error")
}

fn create_error_message(error: &TagCreateError, cx: &Context<TagManager>) -> String {
    let translator = i18n::translator(cx);
    match error {
        TagCreateError::EmptyName => translator.translate("main.form.tag.error.empty_name"),
        TagCreateError::NameTooLong => translator.translate("main.form.tag.error.name_too_long"),
        TagCreateError::DuplicateName(name) => translator.translate_with_args(
            "main.form.tag.error.duplicate_name",
            &[("name", name.clone())],
        ),
        TagCreateError::InvalidColor => translator.translate("main.form.tag.error.invalid_color"),
        TagCreateError::SortOrderOverflow => translator.translate("main.form.tag.error.other"),
        TagCreateError::Database(_) | TagCreateError::Sql(_) => {
            translator.translate("main.form.tag.error.other")
        }
    }
}

fn update_error_message(error: &TagUpdateError, cx: &Context<TagManager>) -> String {
    let translator = i18n::translator(cx);
    match error {
        TagUpdateError::EmptyName => translator.translate("main.form.tag.error.empty_name"),
        TagUpdateError::NameTooLong => translator.translate("main.form.tag.error.name_too_long"),
        TagUpdateError::DuplicateName(name) => translator.translate_with_args(
            "main.form.tag.error.duplicate_name",
            &[("name", name.clone())],
        ),
        TagUpdateError::InvalidColor => translator.translate("main.form.tag.error.invalid_color"),
        TagUpdateError::NotFound(_) | TagUpdateError::Database(_) | TagUpdateError::Sql(_) => {
            translator.translate("main.form.tag.error.other")
        }
    }
}

fn parse_hex_color(value: &str) -> Option<u32> {
    let value = value.trim().trim_start_matches('#');
    if value.len() == 6 && value.chars().all(|character| character.is_ascii_hexdigit()) {
        u32::from_str_radix(value, 16).ok()
    } else {
        None
    }
}

fn hsla_to_rgb(color: Hsla) -> u32 {
    let color = gpui::hsla_to_rgba(color);
    (color.color.red * 255.0).round() as u32 * 0x010000
        + (color.color.green * 255.0).round() as u32 * 0x000100
        + (color.color.blue * 255.0).round() as u32
}

fn format_timestamp(timestamp: i64) -> String {
    Local
        .timestamp_millis_opt(timestamp)
        .single()
        .map(|value| value.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "—".to_owned())
}
