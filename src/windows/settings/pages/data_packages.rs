use super::super::{SettingsWindowView, open_settings_directory};
use crate::{
    i18n,
    services::{
        data_management::{
            self, DataManagementService,
            package::{CloudPackage, ExportOptions, ImportMode, PreparedPackage, Scope},
        },
        sync::SyncService,
    },
    ui::scale::ui_rem,
    windows::{
        main::MainWindowService,
        notification::{NotificationLevel, NotificationService},
    },
};
use gpui::{
    AnyElement, App, AppContext, Context, Entity, FontWeight, Render, Window, div, prelude::*,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputState},
    scroll::ScrollableElement as _,
    v_flex,
};
use std::{path::PathBuf, sync::Arc};
use zeroize::Zeroizing;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum PanelMode {
    Export,
    Import,
    Cloud,
}

struct PackagePanel {
    host: Entity<SettingsWindowView>,
    mode: PanelMode,
    options: ExportOptions,
    days: Entity<InputState>,
    password: Entity<InputState>,
    confirmation: Entity<InputState>,
    encrypt: bool,
    cloud_export: bool,
    cloud_available: bool,
    tags: Vec<(i64, String)>,
    files: Vec<CloudPackage>,
    loaded: bool,
    delete_pending: Option<String>,
    source: Option<PathBuf>,
    download: Option<tempfile::TempDir>,
    prepared: Option<Arc<PreparedPackage>>,
    scope: Scope,
    replace: bool,
    replace_confirmed: bool,
    busy: bool,
    error: Option<String>,
    backup: Option<PathBuf>,
    clear_secret: bool,
}

pub(super) fn open(
    host: Entity<SettingsWindowView>,
    mode: PanelMode,
    window: &mut Window,
    cx: &mut App,
) {
    if host.read(cx).data_busy {
        return;
    }
    let panel = create_panel(host, mode, window, cx);
    let content = panel.clone();
    window.open_sheet(cx, move |sheet, _, cx| {
        sheet
            .title(tr(cx, "package.title"))
            .size(ui_rem(540.0))
            .resizable(false)
            .shadow_none()
            .map(|this| crate::window_material::sheet_surface(this, cx))
            .on_close({
                let panel = content.clone();
                move |_, window, cx| {
                    panel.update(cx, |panel, cx| panel.clear_password(window, cx));
                }
            })
            .child(content.clone())
    });
    if mode == PanelMode::Cloud && panel.read(cx).cloud_available {
        refresh_cloud(panel, cx);
    }
}

fn create_panel(
    host: Entity<SettingsWindowView>,
    mode: PanelMode,
    window: &mut Window,
    cx: &mut App,
) -> Entity<PackagePanel> {
    let service = cx.global::<DataManagementService>().clone();
    let available = SyncService::snapshot(cx)
        .config
        .is_some_and(|c| !c.dav.url.trim().is_empty());
    let tags = match service.package_tags() {
        Ok(tags) => tags,
        Err(error) => {
            tracing::warn!("读取导出标签失败：{error}");
            Vec::new()
        }
    };
    cx.new(|cx| PackagePanel {
        host,
        mode,
        options: ExportOptions {
            scope: Scope {
                favorites: true,
                ..Default::default()
            },
            ..Default::default()
        },
        days: cx.new(|cx| InputState::new(window, cx).placeholder(tr(cx, "package.all_history"))),
        password: cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(tr(cx, "package.password"))
        }),
        confirmation: cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(tr(cx, "package.password_confirm"))
        }),
        encrypt: false,
        cloud_export: false,
        cloud_available: available,
        tags,
        files: Vec::new(),
        loaded: false,
        delete_pending: None,
        source: None,
        download: None,
        prepared: None,
        scope: Scope::default(),
        replace: false,
        replace_confirmed: false,
        busy: false,
        error: None,
        backup: None,
        clear_secret: false,
    })
}

impl PackagePanel {
    fn clear_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.password
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.confirmation
            .update(cx, |input, cx| input.set_value("", window, cx));
    }
    fn switch(&mut self, mode: PanelMode, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = mode;
        self.error = None;
        self.prepared = None;
        self.source = None;
        self.download = None;
        self.replace = false;
        self.replace_confirmed = false;
        self.clear_password(window, cx);
        cx.notify();
    }
    fn scope_controls(&self, export: bool, cx: &mut Context<Self>) -> AnyElement {
        let scope = if export {
            &self.options.scope
        } else {
            &self.scope
        };
        let available = self.prepared.as_ref().map(|p| &p.manifest.scope);
        let mut content = v_flex().gap_3();
        for (index, key, checked) in [
            (0, "package.favorites", scope.favorites),
            (1, "package.history", scope.history),
            (2, "package.gallery", scope.gallery),
            (3, "package.settings", scope.settings),
        ] {
            let present = available.is_none_or(|s| match index {
                0 => s.favorites,
                1 => s.history,
                2 => s.gallery,
                _ => s.settings,
            });
            if !export && !present {
                continue;
            }
            let panel = cx.entity();
            content = content.child(
                Checkbox::new(("package-scope", index as usize))
                    .label(tr(cx, key))
                    .checked(checked)
                    .disabled(self.busy)
                    .on_click(move |checked, _, cx| {
                        panel.update(cx, |panel, cx| {
                            let scope = if export {
                                &mut panel.options.scope
                            } else {
                                &mut panel.scope
                            };
                            match index {
                                0 => scope.favorites = *checked,
                                1 => scope.history = *checked,
                                2 => scope.gallery = *checked,
                                _ => scope.settings = *checked,
                            }
                            panel.replace_confirmed = false;
                            cx.notify();
                        });
                    }),
            );
        }
        content.into_any_element()
    }
    fn help(&self, key: &str, cx: &App) -> AnyElement {
        div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(tr(cx, key))
            .into_any_element()
    }
    fn export_form(&self, cx: &mut Context<Self>) -> AnyElement {
        let panel = cx.entity();
        let cloud = cx.entity();
        let encrypt = cx.entity();
        let mut content = v_flex()
            .gap_4()
            .child(self.help("package.export_help", cx))
            .child(self.scope_controls(true, cx));
        if self.options.scope.history {
            content = content.child(
                v_flex()
                    .gap_2()
                    .child(self.help("package.history_days", cx))
                    .child(Input::new(&self.days).small().disabled(self.busy)),
            );
        }
        if self.options.scope.favorites && !self.tags.is_empty() {
            let mut tags = h_flex().gap_3().flex_wrap();
            for (id, name) in &self.tags {
                let panel = panel.clone();
                let id = *id;
                tags = tags.child(
                    Checkbox::new(("package-tag", id as usize))
                        .label(name.clone())
                        .checked(self.options.favorite_tags.contains(&id))
                        .disabled(self.busy)
                        .on_click(move |checked, _, cx| {
                            panel.update(cx, |panel, cx| {
                                panel.options.favorite_tags.retain(|tag| *tag != id);
                                if *checked {
                                    panel.options.favorite_tags.push(id);
                                }
                                cx.notify();
                            });
                        }),
                );
            }
            content = content.child(
                v_flex()
                    .gap_2()
                    .child(self.help("package.tags_help", cx))
                    .child(tags),
            );
        }
        content = content
            .child(self.help("package.file_references", cx))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("package-target-local")
                            .secondary()
                            .small()
                            .selected(!self.cloud_export)
                            .disabled(self.busy)
                            .label(tr(cx, "package.local"))
                            .on_click(move |_, _, cx| {
                                panel.update(cx, |panel, cx| {
                                    panel.cloud_export = false;
                                    cx.notify();
                                });
                            }),
                    )
                    .child(
                        Button::new("package-target-cloud")
                            .secondary()
                            .small()
                            .selected(self.cloud_export)
                            .disabled(self.busy || !self.cloud_available)
                            .label(tr(cx, "package.webdav"))
                            .on_click(move |_, _, cx| {
                                cloud.update(cx, |panel, cx| {
                                    panel.cloud_export = true;
                                    panel.encrypt = true;
                                    cx.notify();
                                });
                            }),
                    ),
            )
            .child(
                Checkbox::new("package-encrypt")
                    .label(tr(cx, "package.encrypt"))
                    .checked(self.encrypt)
                    .disabled(self.busy)
                    .on_click(move |checked, _, cx| {
                        encrypt.update(cx, |panel, cx| {
                            panel.encrypt = *checked;
                            cx.notify();
                        });
                    }),
            );
        if !self.cloud_available {
            content = content.child(self.help("package.configure_webdav", cx));
        }
        if self.encrypt {
            content = content
                .child(self.help("package.password_help", cx))
                .child(
                    Input::new(&self.password)
                        .mask_toggle()
                        .small()
                        .disabled(self.busy),
                )
                .child(
                    Input::new(&self.confirmation)
                        .mask_toggle()
                        .small()
                        .disabled(self.busy),
                );
        } else {
            content = content.child(self.help("package.unencrypted_help", cx));
        }
        content.into_any_element()
    }
    fn import_form(&self, cx: &mut Context<Self>) -> AnyElement {
        let select = cx.entity();
        let mut content = v_flex().gap_4();
        if let Some(path) = &self.source {
            let change = cx.entity();
            content = content.child(
                h_flex()
                    .w_full()
                    .gap_3()
                    .p_3()
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded_md()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .truncate()
                                    .child(
                                        path.file_name()
                                            .unwrap_or_default()
                                            .to_string_lossy()
                                            .to_string(),
                                    ),
                            )
                            .child(self.help(
                                if self.download.is_some() {
                                    "package.source_cloud"
                                } else {
                                    "package.source_local"
                                },
                                cx,
                            )),
                    )
                    .child(
                        Button::new("package-change-file")
                            .ghost()
                            .small()
                            .flex_shrink_0()
                            .disabled(self.busy)
                            .label(tr(cx, "package.change_file"))
                            .on_click(move |_, window, cx| {
                                change.update(cx, |panel, cx| {
                                    panel.switch(PanelMode::Import, window, cx);
                                });
                            }),
                    ),
            );
        } else {
            content = content.child(self.help("package.import_help", cx)).child(
                Button::new("package-choose-file")
                    .secondary()
                    .small()
                    .disabled(self.busy)
                    .label(tr(cx, "package.choose_file"))
                    .on_click(move |_, _, cx| choose_local(select.clone(), cx)),
            );
        }
        if let Some(prepared) = &self.prepared {
            let m = &prepared.manifest;
            let t = i18n::translator(cx);
            let mut summary = vec![format_time(m.created_at)];
            if m.scope.favorites {
                summary.push(t.translate_with_args(
                    "package.favorite_count",
                    &[("count", m.favorite_count.to_string())],
                ));
            }
            if m.scope.history {
                summary.push(t.translate_with_args(
                    "package.history_count",
                    &[("count", m.history_count.to_string())],
                ));
            }
            if m.scope.gallery {
                summary.push(t.translate("package.gallery"));
            }
            if m.scope.settings {
                summary.push(t.translate("package.settings"));
            }
            content = content
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(summary.join(" · ")),
                )
                .child(self.scope_controls(false, cx));
            if m.filtered {
                content = content.child(self.help("package.filtered_help", cx));
            }
            let merge = cx.entity();
            let replace = cx.entity();
            content = content
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("package-merge")
                                .secondary()
                                .small()
                                .selected(!self.replace)
                                .disabled(self.busy)
                                .label(t.translate("package.merge"))
                                .on_click(move |_, _, cx| {
                                    merge.update(cx, |panel, cx| {
                                        panel.replace = false;
                                        panel.replace_confirmed = false;
                                        cx.notify();
                                    });
                                }),
                        )
                        .child(
                            Button::new("package-replace")
                                .secondary()
                                .small()
                                .selected(self.replace)
                                .disabled(self.busy)
                                .label(t.translate("package.replace"))
                                .on_click(move |_, _, cx| {
                                    replace.update(cx, |panel, cx| {
                                        panel.replace = true;
                                        panel.replace_confirmed = false;
                                        cx.notify();
                                    });
                                }),
                        ),
                )
                .child(self.help(
                    if self.replace {
                        "package.replace_help"
                    } else {
                        "package.merge_help"
                    },
                    cx,
                ));
            if self.scope.settings {
                content = content.child(self.help("package.settings_help", cx));
            }
            if self.replace {
                let confirm = cx.entity();
                content = content.child(self.help("package.metadata_help", cx)).child(
                    Checkbox::new("package-confirm-replace")
                        .label(t.translate("package.confirm_replace"))
                        .checked(self.replace_confirmed)
                        .disabled(self.busy)
                        .on_click(move |checked, _, cx| {
                            confirm.update(cx, |panel, cx| {
                                panel.replace_confirmed = *checked;
                                cx.notify();
                            });
                        }),
                );
            }
        } else if self.source.is_some() {
            content = content.child(self.help("package.unlock_help", cx)).child(
                Input::new(&self.password)
                    .mask_toggle()
                    .small()
                    .disabled(self.busy),
            );
        }
        content.into_any_element()
    }
    fn cloud_form(&self, cx: &mut Context<Self>) -> AnyElement {
        let refresh = cx.entity();
        let mut content = v_flex().gap_3().child(self.help("package.cloud_help", cx));
        if !self.cloud_available {
            return content
                .child(self.help("package.configure_webdav", cx))
                .into_any_element();
        }
        content = content.child(
            Button::new("package-refresh")
                .secondary()
                .small()
                .disabled(self.busy)
                .label(tr(cx, "package.refresh"))
                .on_click(move |_, _, cx| refresh_cloud(refresh.clone(), cx)),
        );
        if self.loaded && self.files.is_empty() {
            content = content.child(self.help("package.cloud_empty", cx));
        }
        for (index, file) in self.files.iter().enumerate() {
            let download = cx.entity();
            let delete = cx.entity();
            let cancel = cx.entity();
            let item = file.clone();
            let remove = file.clone();
            let pending = self.delete_pending.as_ref() == Some(&file.name);
            let mut row = v_flex()
                .gap_2()
                .p_3()
                .border_1()
                .border_color(cx.theme().border)
                .rounded_md()
                .child(div().text_sm().child(format!(
                    "{} · {}",
                    format_time(file.created_at),
                    super::data_management::format_bytes(file.bytes)
                )))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .truncate()
                        .child(file.name.clone()),
                );
            let mut actions = h_flex().gap_2().child(
                Button::new(("package-download", index))
                    .secondary()
                    .small()
                    .disabled(self.busy)
                    .label(tr(cx, "package.download_preview"))
                    .on_click(move |_, _, cx| download_cloud(download.clone(), item.clone(), cx)),
            );
            if pending {
                actions = actions
                    .child(
                        Button::new(("package-delete-confirm", index))
                            .danger()
                            .small()
                            .disabled(self.busy)
                            .label(tr(cx, "package.delete_confirm"))
                            .on_click(move |_, _, cx| {
                                delete_cloud(delete.clone(), remove.clone(), cx)
                            }),
                    )
                    .child(
                        Button::new(("package-delete-cancel", index))
                            .ghost()
                            .small()
                            .disabled(self.busy)
                            .label(tr(cx, "package.cancel"))
                            .on_click(move |_, _, cx| {
                                cancel.update(cx, |panel, cx| {
                                    panel.delete_pending = None;
                                    cx.notify();
                                });
                            }),
                    );
            } else {
                actions = actions.child(
                    Button::new(("package-delete", index))
                        .ghost()
                        .small()
                        .disabled(self.busy)
                        .label(tr(cx, "package.delete"))
                        .on_click(move |_, _, cx| {
                            delete.update(cx, |panel, cx| {
                                panel.delete_pending = Some(remove.name.clone());
                                cx.notify();
                            });
                        }),
                );
            }
            row = row.child(actions);
            content = content.child(row);
        }
        content.into_any_element()
    }
}

impl Render for PackagePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.clear_secret {
            self.clear_password(window, cx);
            self.clear_secret = false;
        }
        let mut tabs = h_flex().gap_2();
        for (index, mode, key) in [
            (0, PanelMode::Export, "package.export"),
            (1, PanelMode::Import, "package.import"),
            (2, PanelMode::Cloud, "package.cloud"),
        ] {
            let panel = cx.entity();
            tabs = tabs.child(
                Button::new(("package-tab", index as usize))
                    .secondary()
                    .small()
                    .selected(self.mode == mode)
                    .disabled(self.busy)
                    .label(tr(cx, key))
                    .on_click(move |_, window, cx| {
                        panel.update(cx, |panel, cx| panel.switch(mode, window, cx));
                        if mode == PanelMode::Cloud
                            && panel.read(cx).cloud_available
                            && !panel.read(cx).loaded
                        {
                            refresh_cloud(panel.clone(), cx);
                        }
                    }),
            );
        }
        let body = match self.mode {
            PanelMode::Export => self.export_form(cx),
            PanelMode::Import => self.import_form(cx),
            PanelMode::Cloud => self.cloud_form(cx),
        };
        let mut content = v_flex()
            .id("package-content")
            .flex_1()
            .min_h_0()
            .gap_4()
            .overflow_y_scrollbar()
            .child(body);
        if let Some(error) = &self.error {
            content = content.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(error.clone()),
            );
        }
        if let Some(backup) = &self.backup {
            let path = backup.parent().unwrap_or(backup).to_path_buf();
            content = content.child(
                Button::new("package-open-backup")
                    .secondary()
                    .small()
                    .label(tr(cx, "package.open_backup"))
                    .on_click(move |_, _, cx| {
                        open_settings_directory(path.clone(), "package.title", cx)
                    }),
            );
        }
        let mut root = v_flex()
            .size_full()
            .min_h_0()
            .gap_4()
            .p_4()
            .child(tabs)
            .child(content);
        if !self.cloud_available && self.mode != PanelMode::Import {
            let host = self.host.clone();
            root = root.child(
                Button::new("package-configure-webdav")
                    .secondary()
                    .small()
                    .disabled(self.busy)
                    .label(tr(cx, "package.configure_webdav_action"))
                    .on_click(move |_, window, cx| {
                        window.close_sheet(cx);
                        host.update(cx, |view, cx| {
                            view.active_page = super::super::SettingsPage::Sync;
                            cx.notify();
                        });
                        super::sync::open_webdav(host.clone(), window, cx);
                    }),
            );
        }
        if self.busy {
            root = root.child(div().text_sm().child(tr(cx, "package.working")));
        } else if self.mode != PanelMode::Cloud
            && (self.mode == PanelMode::Export || self.source.is_some())
        {
            let panel = cx.entity();
            let is_export = self.mode == PanelMode::Export;
            let ready = if is_export {
                self.options.scope.any()
            } else if self.prepared.is_some() {
                self.scope.any() && (!self.replace || self.replace_confirmed)
            } else {
                self.source.is_some()
            };
            let key = if is_export {
                if self.cloud_export {
                    "package.upload"
                } else {
                    "package.save"
                }
            } else if self.prepared.is_some() {
                "package.apply"
            } else {
                "package.unlock"
            };
            if !is_export && self.prepared.is_some() {
                root = root.child(self.help("package.backup_help", cx));
            }
            root = root.child(
                Button::new("package-primary")
                    .primary()
                    .disabled(!ready)
                    .label(tr(cx, key))
                    .w_full()
                    .on_click(move |_, _, cx| {
                        if is_export {
                            export(panel.clone(), cx);
                        } else if panel.read(cx).prepared.is_some() {
                            apply(panel.clone(), cx);
                        } else {
                            inspect(panel.clone(), cx);
                        }
                    }),
            );
        }
        root
    }
}

fn format_time(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp_millis(timestamp)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| "—".into())
}
fn service(cx: &App) -> DataManagementService {
    cx.global::<DataManagementService>().clone()
}
fn begin(panel: &Entity<PackagePanel>, blocking: bool, cx: &mut App) -> bool {
    if panel.read(cx).busy {
        return false;
    }
    if blocking && !MainWindowService::begin_blocking_operation("package.title", cx) {
        return false;
    }
    let host = panel.read(cx).host.clone();
    host.update(cx, |view, cx| {
        view.data_busy = true;
        cx.notify();
    });
    panel.update(cx, |panel, cx| {
        panel.busy = true;
        panel.error = None;
        cx.notify();
    });
    true
}
fn finish(
    panel: &Entity<PackagePanel>,
    blocking: bool,
    result: Result<(), String>,
    success: Option<&str>,
    cx: &mut App,
) {
    let host = panel.read(cx).host.clone();
    host.update(cx, |view, cx| {
        view.data_busy = false;
        cx.notify();
    });
    panel.update(cx, |panel, cx| {
        panel.busy = false;
        if result.is_ok() && success.is_some() {
            panel.clear_secret = true;
        }
        panel.error = result.err();
        cx.notify();
    });
    if blocking {
        MainWindowService::finish_blocking_operation(cx);
    }
    if let Some(error) = &panel.read(cx).error {
        tracing::warn!("数据包操作失败：{error}");
        NotificationService::show_message(
            tr(cx, "package.title"),
            error.clone(),
            NotificationLevel::Error,
            cx,
        );
    } else if let Some(key) = success {
        NotificationService::show_message(
            tr(cx, "package.title"),
            tr(cx, key),
            NotificationLevel::Success,
            cx,
        );
    }
}
fn show_error(panel: &Entity<PackagePanel>, key: &str, cx: &mut App) {
    panel.update(cx, |panel, cx| {
        panel.error = Some(tr(cx, key));
        cx.notify();
    });
}
fn choose_local(panel: Entity<PackagePanel>, cx: &mut App) {
    if !begin(&panel, false, cx) {
        return;
    }
    let title = tr(cx, "package.choose_file");
    cx.spawn(async move |cx| {
        let file = rfd::AsyncFileDialog::new()
            .set_title(title)
            .add_filter("QcClip", &["qcclip-data"])
            .pick_file()
            .await;
        cx.update(|cx| {
            let selected = file.is_some();
            panel.update(cx, |panel, cx| {
                if let Some(file) = file {
                    panel.source = Some(file.path().to_path_buf());
                    panel.prepared = None;
                    panel.download = None;
                }
                cx.notify();
            });
            finish(&panel, false, Ok(()), None, cx);
            if selected {
                inspect(panel, cx);
            }
        });
    })
    .detach();
}
fn inspect(panel: Entity<PackagePanel>, cx: &mut App) {
    let Some(path) = panel.read(cx).source.clone() else {
        return;
    };
    let password = Zeroizing::new(panel.read(cx).password.read(cx).value().to_string());
    let service = service(cx);
    if !begin(&panel, false, cx) {
        return;
    }
    cx.spawn(async move |cx| {
        let result = cx
            .background_executor()
            .spawn(async move {
                if DataManagementService::package_is_encrypted(&path)? && password.is_empty() {
                    return Ok(None);
                }
                service
                    .inspect_package(&path, (!password.is_empty()).then_some(password.as_str()))
                    .map(|p| Some(Arc::new(p)))
            })
            .await;
        cx.update(|cx| {
            let result = result
                .map(|prepared| {
                    panel.update(cx, |panel, cx| {
                        if let Some(prepared) = prepared {
                            panel.scope = prepared.manifest.scope.clone();
                            panel.prepared = Some(prepared);
                            panel.clear_secret = true;
                            panel.replace = false;
                            panel.replace_confirmed = false;
                        }
                        cx.notify();
                    });
                })
                .map_err(|e: data_management::package::Error| e.to_string());
            finish(&panel, false, result, None, cx);
        });
    })
    .detach();
}
fn export(panel: Entity<PackagePanel>, cx: &mut App) {
    let state = panel.read(cx);
    let mut options = state.options.clone();
    if options.scope.history {
        let days = state.days.read(cx).value();
        if !days.trim().is_empty() {
            let Some(days) = days
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|d| (1..=36500).contains(d))
            else {
                show_error(&panel, "package.invalid_days", cx);
                return;
            };
            options.history_days = Some(days);
        }
    }
    let password = state
        .encrypt
        .then(|| Zeroizing::new(state.password.read(cx).value().to_string()));
    if let Some(password) = &password
        && (password.is_empty()
            || password.as_str() != state.confirmation.read(cx).value().as_ref())
    {
        show_error(&panel, "package.password_mismatch", cx);
        return;
    }
    let cloud = state.cloud_export;
    let client = if cloud {
        match SyncService::get(cx).and_then(|r| r.package_client()) {
            Ok(c) => Some(c),
            Err(error) => {
                panel.update(cx, |p, cx| {
                    p.error = Some(error.to_string());
                    cx.notify();
                });
                return;
            }
        }
    } else {
        None
    };
    let service = service(cx);
    let name = service.suggested_export_file_name();
    let title = tr(cx, "package.save");
    if !begin(&panel, false, cx) {
        return;
    }
    cx.spawn(async move |cx| {
        let temporary = if cloud {
            tempfile::tempdir().map(Some)
        } else {
            Ok(None)
        };
        let result = match temporary {
            Err(error) => Err(error.to_string()),
            Ok(temporary) => {
                let path = if let Some(dir) = &temporary {
                    Some(dir.path().join("export.qcclip-data"))
                } else {
                    rfd::AsyncFileDialog::new()
                        .set_title(title)
                        .set_file_name(name)
                        .add_filter("QcClip", &["qcclip-data"])
                        .save_file()
                        .await
                        .map(|f| f.path().to_path_buf())
                };
                if let Some(path) = path {
                    cx.background_executor()
                        .spawn(async move {
                            service.export_package(
                                &path,
                                &options,
                                password.as_ref().map(|p| p.as_str()),
                            )?;
                            if let Some(client) = client {
                                CloudPackage::upload(&client, &path)?;
                            }
                            Ok::<_, data_management::package::Error>(())
                        })
                        .await
                        .map_err(|e| e.to_string())
                        .map(|_| true)
                } else {
                    Ok(false)
                }
            }
        };
        cx.update(|cx| {
            let success = result.as_ref().ok().is_some_and(|done| *done);
            finish(
                &panel,
                false,
                result.map(|_| ()),
                success.then_some(if cloud {
                    "package.upload_success"
                } else {
                    "package.export_success"
                }),
                cx,
            );
        });
    })
    .detach();
}
fn apply(panel: Entity<PackagePanel>, cx: &mut App) {
    let state = panel.read(cx);
    let Some(prepared) = state.prepared.clone() else {
        return;
    };
    if state.replace && !state.replace_confirmed {
        return;
    }
    let scope = state.scope.clone();
    let mode = if state.replace {
        ImportMode::Replace
    } else {
        ImportMode::Merge
    };
    let service = service(cx);
    let root = service.data_root().to_path_buf();
    if !begin(&panel, true, cx) {
        return;
    }
    crate::services::quick_favorite::QuickFavoriteService::suspend(cx);
    MainWindowService::stop_clipboard_listener(cx);
    cx.spawn(async move |cx| {
        let result = cx
            .background_executor()
            .spawn(async move { service.import_package(&prepared, &scope, mode) })
            .await;
        cx.update(|cx| {
            let result = match result {
                Ok((backup, guard)) => {
                    let reload = data_management::reload_runtime_after_import(&root, cx);
                    drop(guard);
                    data_management::refresh_after_import(reload, cx);
                    panel.update(cx, |panel, cx| {
                        panel.backup = Some(backup);
                        panel.prepared = None;
                        panel.source = None;
                        panel.download = None;
                        cx.notify();
                    });
                    let host = panel.read(cx).host.clone();
                    host.update(cx, |view, cx| {
                        view.data_stats = None;
                        view.data_stats_policy = None;
                        view.data_stats_loading = false;
                        view.data_stats_request_id = view.data_stats_request_id.wrapping_add(1);
                        view.refresh_data_stats(cx);
                        cx.notify();
                    });
                    Ok(())
                }
                Err(error) => {
                    MainWindowService::restart_clipboard_listener(cx);
                    Err(error.to_string())
                }
            };
            crate::services::quick_favorite::QuickFavoriteService::resume(cx);
            finish(&panel, true, result, Some("package.import_success"), cx);
        });
    })
    .detach();
}
fn refresh_cloud(panel: Entity<PackagePanel>, cx: &mut App) {
    let client = match SyncService::get(cx).and_then(|r| r.package_client()) {
        Ok(c) => c,
        Err(error) => {
            panel.update(cx, |p, cx| {
                p.error = Some(error.to_string());
                cx.notify();
            });
            return;
        }
    };
    if !begin(&panel, false, cx) {
        return;
    }
    cx.spawn(async move |cx| {
        let result = cx
            .background_executor()
            .spawn(async move { CloudPackage::list(&client) })
            .await;
        cx.update(|cx| {
            let result = result
                .map(|files| {
                    panel.update(cx, |p, cx| {
                        p.files = files;
                        p.loaded = true;
                        p.delete_pending = None;
                        cx.notify();
                    });
                })
                .map_err(|e| e.to_string());
            finish(&panel, false, result, None, cx);
        });
    })
    .detach();
}
fn download_cloud(panel: Entity<PackagePanel>, file: CloudPackage, cx: &mut App) {
    let client = match SyncService::get(cx).and_then(|r| r.package_client()) {
        Ok(c) => c,
        Err(error) => {
            panel.update(cx, |p, cx| {
                p.error = Some(error.to_string());
                cx.notify();
            });
            return;
        }
    };
    if !begin(&panel, false, cx) {
        return;
    }
    cx.spawn(async move |cx| {
        let result = cx
            .background_executor()
            .spawn(async move {
                let dir = tempfile::tempdir()?;
                let path = dir.path().join(&file.name);
                file.download(&client, &path)?;
                Ok::<_, data_management::package::Error>((dir, path))
            })
            .await;
        let downloaded = result.is_ok();
        cx.update(|cx| {
            let result = result
                .map(|(dir, path)| {
                    panel.update(cx, |p, cx| {
                        p.download = Some(dir);
                        p.source = Some(path);
                        p.prepared = None;
                        p.mode = PanelMode::Import;
                        cx.notify();
                    });
                })
                .map_err(|e| e.to_string());
            finish(&panel, false, result, None, cx);
        });
        if downloaded {
            cx.update(|cx| inspect(panel, cx));
        }
    })
    .detach();
}
fn delete_cloud(panel: Entity<PackagePanel>, file: CloudPackage, cx: &mut App) {
    let client = match SyncService::get(cx).and_then(|r| r.package_client()) {
        Ok(c) => c,
        Err(error) => {
            panel.update(cx, |p, cx| {
                p.error = Some(error.to_string());
                cx.notify();
            });
            return;
        }
    };
    if !begin(&panel, false, cx) {
        return;
    }
    cx.spawn(async move |cx| {
        let name = file.name.clone();
        let result = cx
            .background_executor()
            .spawn(async move { file.delete(&client) })
            .await;
        cx.update(|cx| {
            let result = result
                .map(|_| {
                    panel.update(cx, |p, cx| {
                        p.files.retain(|f| f.name != name);
                        p.delete_pending = None;
                        cx.notify();
                    });
                })
                .map_err(|e| e.to_string());
            finish(&panel, false, result, Some("package.delete_success"), cx);
        });
    })
    .detach();
}

fn tr(cx: &App, key: &str) -> String {
    i18n::translator(cx).translate(key)
}
