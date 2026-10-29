use gpui::prelude::*;
use gpui::{App, Entity, Window, div};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::switch::Switch;
use gpui_component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};

use super::super::{SettingsWindowView, components::compact_setting_row};
use super::personalization::persist_appearance_change;
use crate::{
    config::{ItemDisplayField, ItemDisplayPreset, ItemRegionVisibility},
    config_runtime, i18n,
    ui::{SingleSelectOption, scale::ui_rem, single_select},
};

pub(super) fn open_item_display_sheet(
    view: Entity<SettingsWindowView>,
    window: &mut Window,
    cx: &mut App,
) {
    window.open_sheet(cx, move |sheet, _, cx| {
        let theme = cx.theme().colors;
        let translator = i18n::translator(cx);
        let appearance = config_runtime::appearance_config(cx);
        let display = &appearance.item_display;
        let save_error = translator.translate("settings.personalization.save_error");
        let mut presets = h_flex().w_full().flex_shrink_0().gap_2().pb_2();
        for preset in ItemDisplayPreset::ALL {
            presets = presets.child(
                Button::new(format!("item-display-preset-{preset:?}"))
                    .small()
                    .label(translator.translate(preset.label_key()))
                    .when(display.preset() == Some(preset), |button| button.primary())
                    .on_click({
                        let view = view.clone();
                        let save_error = save_error.clone();
                        move |_, _, cx| {
                            persist_appearance_change(
                                view.clone(),
                                "条目显示预设",
                                move |appearance| appearance.item_display = preset.config(),
                                save_error.clone(),
                                cx,
                            )
                        }
                    }),
            );
        }
        let mut content = v_flex()
            .size_full()
            .min_h_0()
            .overflow_y_scrollbar()
            .child(presets);
        for (group, title_key, visibility, fields) in [
            (
                "information",
                "settings.item_display.information",
                display.info_visibility,
                vec![
                    ItemDisplayField::Time,
                    ItemDisplayField::Length,
                    ItemDisplayField::ImageSize,
                ],
            ),
            (
                "identity",
                "settings.item_display.toolbar",
                display.toolbar_visibility,
                vec![ItemDisplayField::Source, ItemDisplayField::Shortcut],
            ),
            (
                "actions",
                "settings.item_display.actions",
                display.actions_visibility,
                vec![
                    ItemDisplayField::Edit,
                    ItemDisplayField::PinToScreen,
                    ItemDisplayField::SplitWords,
                    ItemDisplayField::Note,
                    ItemDisplayField::PastePlain,
                    ItemDisplayField::OpenDefault,
                    ItemDisplayField::OpenBrowser,
                    ItemDisplayField::Favorite,
                    ItemDisplayField::Pin,
                    ItemDisplayField::Delete,
                ],
            ),
        ] {
            let selector = single_select(
                format!("item-display-visibility-{group}"),
                ItemRegionVisibility::ALL.into_iter().map(|mode| {
                    SingleSelectOption::new(mode, translator.translate(mode.label_key()))
                }),
                visibility,
                {
                    let view = view.clone();
                    let save_error = save_error.clone();
                    move |mode, cx| {
                        persist_appearance_change(
                            view.clone(),
                            "条目区域显示方式",
                            move |appearance| match group {
                                "information" => appearance.item_display.info_visibility = mode,
                                "identity" => appearance.item_display.toolbar_visibility = mode,
                                _ => appearance.item_display.actions_visibility = mode,
                            },
                            save_error.clone(),
                            cx,
                        )
                    }
                },
                theme,
                config_runtime::animation_enabled(
                    cx,
                    crate::config::AnimationKind::SingleSelectIndicator,
                ),
            );
            content = content.child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .min_h(ui_rem(52.0))
                    .flex_shrink_0()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_4()
                    .py_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .min_w_0()
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(translator.translate(title_key)),
                    )
                    .child(div().flex_none().max_w_full().child(selector)),
            );
            for field in fields {
                let label = translator.translate(field.label_key());
                let switch = Switch::new(format!("item-display-field-{field:?}"))
                    .checked(display.enabled(field))
                    .accessibility_label(label.clone())
                    .on_click({
                        let view = view.clone();
                        let save_error = save_error.clone();
                        move |checked, _, cx| {
                            persist_appearance_change(
                                view.clone(),
                                "条目显示内容",
                                move |appearance| {
                                    appearance.item_display.set_enabled(field, *checked)
                                },
                                save_error.clone(),
                                cx,
                            )
                        }
                    });
                content = content.child(compact_setting_row(theme, label, switch));
            }
        }
        content = content.child(
            h_flex()
                .w_full()
                .min_h(ui_rem(52.0))
                .flex_shrink_0()
                .items_center()
                .px_4()
                .py_2()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div()
                        .min_w_0()
                        .text_sm()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(translator.translate("settings.item_display.status_border")),
                ),
        );
        for (key, label_key, checked) in [
            (
                "pinned",
                "settings.item_display.pinned_border",
                appearance.pinned_item_border,
            ),
            (
                "pasted",
                "settings.item_display.pasted_border",
                appearance.pasted_item_border,
            ),
        ] {
            let label = translator.translate(label_key);
            let switch = Switch::new(format!("item-display-status-border-{key}"))
                .checked(checked)
                .accessibility_label(label.clone())
                .on_click({
                    let view = view.clone();
                    let save_error = save_error.clone();
                    move |checked, _, cx| {
                        let checked = *checked;
                        persist_appearance_change(
                            view.clone(),
                            "条目状态边框",
                            move |appearance| match key {
                                "pinned" => appearance.pinned_item_border = checked,
                                _ => appearance.pasted_item_border = checked,
                            },
                            save_error.clone(),
                            cx,
                        )
                    }
                });
            content = content.child(compact_setting_row(theme, label, switch));
        }
        sheet
            .title(translator.translate("settings.item_display.title"))
            .size(ui_rem(420.0))
            .resizable(false)
            .shadow_none()
            .map(|this| crate::window_material::sheet_surface(this, cx))
            .child(content)
    });
}
