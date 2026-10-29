use gpui::prelude::*;
use gpui::{App, Context, Entity, Render, Window, div, px};
use gpui_component::{
    ActiveTheme as _, Disableable as _,
    input::{Textarea, TextareaState},
};

use crate::{
    domain::clipboard::ClipboardItemId,
    i18n,
    services::database::NoteRepository,
    windows::dialog::{
        DialogMode, DialogWindowPosition, dialog_body, dialog_button, dialog_footer, dialog_header,
        dialog_primary_button, open_adaptive_window,
    },
};

type NoteSavedCallback = Box<dyn FnOnce(&mut App)>;

pub(crate) fn open(
    source_window: Option<&Window>,
    item_id: ClipboardItemId,
    text: String,
    repository: NoteRepository,
    on_saved: impl FnOnce(&mut App) + 'static,
    cx: &mut App,
) -> Result<(), String> {
    open_adaptive_window(
        source_window,
        cx,
        DialogWindowPosition::Center,
        DialogMode::Modal,
        move |window, cx| {
            cx.new(|cx| {
                let editor = cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .default_value(text)
                        .soft_wrap(true)
                });
                let focus_editor = editor.clone();
                window.defer(cx, move |window, cx| {
                    focus_editor.update(cx, |editor, cx| editor.focus(window, cx));
                });
                NoteEditor {
                    item_id,
                    editor,
                    repository,
                    on_saved: Some(Box::new(on_saved)),
                    is_saving: false,
                    save_error: false,
                }
            })
        },
    )
    .map(|_| ())
}

struct NoteEditor {
    item_id: ClipboardItemId,
    editor: Entity<TextareaState>,
    repository: NoteRepository,
    on_saved: Option<NoteSavedCallback>,
    is_saving: bool,
    save_error: bool,
}

impl NoteEditor {
    fn save(&mut self, clear: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_saving {
            return;
        }
        let text = if clear {
            String::new()
        } else {
            self.editor.read(cx).value().to_string()
        };
        let repository = self.repository.clone();
        let item_id = self.item_id;
        self.is_saving = true;
        self.save_error = false;
        cx.notify();
        let task = cx.background_spawn(async move { repository.save(item_id, &text) });
        cx.spawn_in(window, async move |this, cx| match task.await {
            Ok(()) => {
                let _ = this.update_in(cx, |this, window, cx| {
                    if let Some(on_saved) = this.on_saved.take() {
                        on_saved(cx);
                    }
                    window.remove_window();
                });
            }
            Err(error) => {
                tracing::error!("保存条目备注失败：{error}");
                let _ = this.update(cx, |this, cx| {
                    this.is_saving = false;
                    this.save_error = true;
                    cx.notify();
                });
            }
        })
        .detach();
    }
}

impl Render for NoteEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let translator = i18n::translator(cx);
        div()
            .w(px(460.0))
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(dialog_header(
                translator.translate("main.note.title"),
                theme,
            ))
            .child(dialog_body(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(
                        div()
                            .h(px(220.0))
                            .w_full()
                            .border_1()
                            .border_color(theme.input)
                            .rounded(px(4.0))
                            .overflow_hidden()
                            .child(
                                Textarea::new(&self.editor)
                                    .appearance(false)
                                    .bordered(false)
                                    .size_full()
                                    .disabled(self.is_saving),
                            ),
                    )
                    .when(self.save_error, |this| {
                        this.child(
                            div()
                                .text_sm()
                                .text_color(theme.danger)
                                .child(translator.translate("main.note.save_error")),
                        )
                    })
                    .child(dialog_footer(
                        vec![
                            dialog_button("note-clear", translator.translate("main.note.clear"))
                                .disabled(self.is_saving)
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.save(true, window, cx)),
                                )
                                .into_any_element(),
                            dialog_button("note-cancel", translator.translate("main.form.cancel"))
                                .disabled(self.is_saving)
                                .on_click(|_, window, _| window.remove_window())
                                .into_any_element(),
                            dialog_primary_button(
                                "note-save",
                                translator.translate("main.note.save"),
                            )
                            .disabled(self.is_saving)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.save(false, window, cx)),
                            )
                            .into_any_element(),
                        ],
                        theme,
                    )),
            ))
    }
}
