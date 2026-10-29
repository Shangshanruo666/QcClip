use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use gpui::{App, AppContext as _, BorrowAppContext as _, DisplayId, Global, WindowHandle};
use gpui_component::Root;

use crate::{
    config::NotificationKind,
    domain::capture::{CapturedClipboard, FormatKind},
    domain::clipboard::{ClipboardItemId, HistoryItemId, Tag},
    platform::clipboard::{ClipboardWriteSuppressor, capture_selection, captured_plain_text},
    services::{
        capture::{CaptureService, CaptureStoreResult},
        custom_text::CustomTextService,
        data_runtime::{self, DataOperationGuard},
        database::{Database, FavoriteRepository, TagRepository},
        storage::RawBlobStore,
    },
    windows::notification::{NotificationLevel, NotificationService},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct QuickFavoriteSaveResult {
    is_new: bool,
    added: bool,
}

#[derive(Clone, Debug)]
struct BoundFavoriteItem {
    item_id: ClipboardItemId,
    history_id: HistoryItemId,
    original_text: String,
    is_new: bool,
}

#[derive(Clone)]
struct QuickFavoriteRepositories {
    custom_text: CustomTextService,
    favorites: FavoriteRepository,
    tags: TagRepository,
}

/// A token belongs to one data-source session and is never reactivated.
#[derive(Clone)]
struct FavoriteSession(Arc<AtomicBool>);

impl FavoriteSession {
    fn new() -> Self {
        Self(Arc::new(AtomicBool::new(true)))
    }

    fn retire(&self) {
        self.0.store(false, Ordering::Release);
    }

    fn is_current(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    fn begin_operation(&self) -> Result<DataOperationGuard, String> {
        // Acquire the runtime guard before checking the token: replacement must
        // wait for the entire save, including all repository/blob operations.
        let operation = data_runtime::begin_operation().map_err(|error| error.to_string())?;
        if !self.is_current() {
            return Err("数据源已切换，请重新打开快捷收藏".to_owned());
        }
        Ok(operation)
    }

    fn save(
        &self,
        bound: Option<BoundFavoriteItem>,
        text: &str,
        tag_ids: &[i64],
        repositories: &QuickFavoriteRepositories,
    ) -> Result<(QuickFavoriteSaveResult, ClipboardItemId), String> {
        let _operation = self.begin_operation()?;
        match bound {
            Some(bound) => {
                let item_id = bound.item_id;
                persist_bound_favorite(bound, text, tag_ids, repositories)
                    .map(|result| (result, item_id))
            }
            None => persist_manual_favorite(text, tag_ids, repositories),
        }
    }

    fn prepare(
        &self,
        capture_service: &CaptureService,
        capture: impl FnOnce() -> Result<Option<CapturedClipboard>, String>,
        open_editor_on_empty: bool,
        repositories: &QuickFavoriteRepositories,
    ) -> Result<OpenFavorite, String> {
        let _operation = self.begin_operation()?;
        let Some(captured) = capture()? else {
            return if open_editor_on_empty {
                Ok(OpenFavorite::Editor {
                    text: String::new(),
                    bound: None,
                    selected_tag_ids: Vec::new(),
                    tags: repositories
                        .tags
                        .list()
                        .map_err(|error| error.to_string())?,
                })
            } else {
                Ok(OpenFavorite::NoSelection)
            };
        };
        if let Some(format) = captured.formats.iter().find(|format| {
            format.format_kind == FormatKind::Text
                && format.format_key.eq_ignore_ascii_case("CF_UNICODETEXT")
                && (!format.capture_state.has_data() || format.bytes.is_none())
        }) {
            return Err(format!(
                "无法读取选中文本：{}",
                format
                    .error_message
                    .as_deref()
                    .unwrap_or("缺少 CF_UNICODETEXT 原始数据")
            ));
        }
        let store = capture_service
            .store_captured_clipboard_with_status(&captured)
            .map_err(|error| error.to_string())?;
        let Some(text) = captured_plain_text(&captured) else {
            return Ok(OpenFavorite::Saved {
                result: favorite_captured_item(&store, repositories)?,
                history_id: store.history_id,
            });
        };
        Ok(OpenFavorite::Editor {
            bound: Some(BoundFavoriteItem {
                item_id: store.item_id,
                history_id: store.history_id,
                original_text: text.clone(),
                is_new: store.is_new,
            }),
            text,
            selected_tag_ids: repositories
                .tags
                .item_tag_ids(store.item_id)
                .map_err(|error| error.to_string())?,
            tags: repositories
                .tags
                .list()
                .map_err(|error| error.to_string())?,
        })
    }
}

#[derive(Debug)]
enum OpenFavorite {
    NoSelection,
    Editor {
        text: String,
        bound: Option<BoundFavoriteItem>,
        tags: Vec<Tag>,
        selected_tag_ids: Vec<i64>,
    },
    Saved {
        result: QuickFavoriteSaveResult,
        history_id: HistoryItemId,
    },
}

pub(crate) struct QuickFavoriteService {
    repositories: QuickFavoriteRepositories,
    capture_service: CaptureService,
    clipboard_write_suppressor: ClipboardWriteSuppressor,
    editor_window: Option<WindowHandle<Root>>,
    opening: bool,
    session: FavoriteSession,
}

impl Global for QuickFavoriteService {}

impl QuickFavoriteService {
    pub(crate) fn install(
        tag_repository: TagRepository,
        custom_text_service: CustomTextService,
        favorite_repository: FavoriteRepository,
        capture_service: CaptureService,
        clipboard_write_suppressor: ClipboardWriteSuppressor,
        cx: &mut App,
    ) {
        cx.set_global(Self {
            repositories: QuickFavoriteRepositories {
                custom_text: custom_text_service,
                favorites: favorite_repository,
                tags: tag_repository,
            },
            capture_service,
            clipboard_write_suppressor,
            editor_window: None,
            opening: false,
            session: FavoriteSession::new(),
        });
    }

    /// Retire queued work before the data runtime begins replacing files.
    pub(crate) fn suspend(cx: &mut App) {
        cx.update_global::<Self, _>(|service, cx| {
            service.session.retire();
            service.opening = false;
            if let Some(handle) = service.editor_window.take() {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            }
        });
    }

    pub(crate) fn replace_data_sources(database: Database, blob_store: RawBlobStore, cx: &mut App) {
        cx.update_global::<Self, _>(|service, _| service.replace_sources(database, blob_store));
    }

    fn replace_sources(&mut self, database: Database, blob_store: RawBlobStore) {
        self.repositories = QuickFavoriteRepositories {
            custom_text: CustomTextService::new(database.clone(), blob_store.clone()),
            favorites: FavoriteRepository::new(database.clone()),
            tags: TagRepository::new(database.clone()),
        };
        self.capture_service = CaptureService::new(database, blob_store);
    }

    pub(crate) fn resume(cx: &mut App) {
        cx.update_global::<Self, _>(|service, _| {
            if !service.session.is_current() {
                service.session = FavoriteSession::new();
            }
        });
    }

    /// 快捷键与悬浮 + 共享单个编辑器：已有窗口时只聚焦，不重新捕获或新建。
    pub(crate) fn open(from_hotkey: bool, cx: &mut App) {
        if !cx.global::<Self>().session.is_current() || Self::focus_open_editor(cx) {
            return;
        }
        let display_id =
            crate::platform::window::display_id_for_foreground_window().or_else(|| {
                let point = crate::platform::window::physical_cursor_position()?;
                crate::platform::window::display_for_physical_point(cx, point)
                    .map(|placement| placement.display_id)
            });
        let opening = cx.update_global::<Self, _>(|service, _| {
            if service.opening {
                return false;
            }
            service.opening = true;
            true
        });
        if !opening {
            return;
        }

        let (repositories, capture_service, clipboard_write_suppressor, session) = {
            let service = cx.global::<Self>();
            (
                service.repositories.clone(),
                service.capture_service.clone(),
                service.clipboard_write_suppressor.clone(),
                service.session.clone(),
            )
        };
        let open_editor_on_empty = !from_hotkey
            || crate::config_runtime::shortcuts_config(cx).add_favorite_open_editor_on_empty;
        let task_session = session.clone();
        let task = cx.background_spawn(async move {
            task_session.prepare(
                &capture_service,
                || {
                    if from_hotkey {
                        capture_selection(&clipboard_write_suppressor)
                            .map_err(|error| error.to_string())
                    } else {
                        Ok(None)
                    }
                },
                open_editor_on_empty,
                &repositories,
            )
        });
        cx.spawn(async move |cx| {
            let result = task.await;
            cx.update(|cx| {
                if !session.is_current() {
                    return;
                }
                cx.update_global::<Self, _>(|service, _| service.opening = false);
                match result {
                    Ok(selection) => {
                        Self::finish_open(from_hotkey, display_id, selection, session, cx);
                    }
                    Err(error) => handle_save_result(from_hotkey, Err(error), cx),
                }
            });
        })
        .detach();
    }

    fn focus_open_editor(cx: &mut App) -> bool {
        let focused = cx.update_global::<Self, _>(|service, cx| {
            let Some(handle) = service.editor_window else {
                return false;
            };
            if handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
            {
                true
            } else {
                service.editor_window = None;
                false
            }
        });
        if focused {
            cx.activate(true);
        }
        focused
    }

    fn finish_open(
        from_hotkey: bool,
        display_id: Option<DisplayId>,
        selection: OpenFavorite,
        session: FavoriteSession,
        cx: &mut App,
    ) {
        let (editor_text, bound, tags, selected_tag_ids) = match selection {
            OpenFavorite::NoSelection => {
                let translator = crate::i18n::translator(cx);
                NotificationService::show_message_for(
                    NotificationKind::Attention,
                    translator.translate("notification.quick_favorite.failed_title"),
                    translator.translate("notification.quick_favorite.no_selection"),
                    NotificationLevel::Warning,
                    cx,
                );
                return;
            }
            OpenFavorite::Saved { result, history_id } => {
                if from_hotkey {
                    notify_favorite_saved(result, cx);
                }
                crate::windows::main::MainWindowService::refresh_captured_history_item(
                    cx, history_id,
                );
                return;
            }
            OpenFavorite::Editor {
                text,
                bound,
                tags,
                selected_tag_ids,
            } => {
                if let Some(bound) = &bound {
                    crate::windows::main::MainWindowService::refresh_captured_history_item(
                        cx,
                        bound.history_id,
                    );
                }
                (text, bound, tags, selected_tag_ids)
            }
        };
        let repositories = cx.global::<Self>().repositories.clone();
        let on_confirm = Box::new(move |text: String, tag_ids: Vec<i64>, cx: &mut App| {
            if session.is_current() {
                let task_session = session.clone();
                let task = cx.background_spawn(async move {
                    task_session.save(bound, &text, &tag_ids, &repositories)
                });
                finish_save_in_background(from_hotkey, session, task, cx);
            }
        });
        let opened = crate::windows::custom_text_editor::open_new_favorite(
            editor_text,
            tags,
            selected_tag_ids,
            display_id,
            on_confirm,
            cx,
        );
        match opened {
            Ok(handle) => {
                cx.update_global::<Self, _>(|service, _| service.editor_window = Some(handle));
            }
            Err(error) => handle_save_result(
                from_hotkey,
                Err(format!("打开快捷收藏窗口失败：{error}")),
                cx,
            ),
        }
    }
}

fn finish_save_in_background(
    from_hotkey: bool,
    session: FavoriteSession,
    task: gpui::Task<Result<(QuickFavoriteSaveResult, ClipboardItemId), String>>,
    cx: &mut App,
) {
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| {
            if session.is_current() {
                handle_save_result(from_hotkey, result, cx);
            }
        });
    })
    .detach();
}

fn persist_bound_favorite(
    bound: BoundFavoriteItem,
    text: &str,
    tag_ids: &[i64],
    repositories: &QuickFavoriteRepositories,
) -> Result<QuickFavoriteSaveResult, String> {
    let added = if text == bound.original_text {
        repositories
            .favorites
            .add_to_favorites_with_tags(bound.item_id, tag_ids)
            .map_err(|error| error.to_string())?
    } else {
        repositories
            .custom_text
            .save_for_item_and_favorite_with_tags(bound.item_id, text, tag_ids)
            .map_err(|error| error.to_string())?
    };
    Ok(QuickFavoriteSaveResult {
        is_new: bound.is_new,
        added,
    })
}

fn persist_manual_favorite(
    text: &str,
    tag_ids: &[i64],
    repositories: &QuickFavoriteRepositories,
) -> Result<(QuickFavoriteSaveResult, ClipboardItemId), String> {
    let item_id = match repositories
        .custom_text
        .find_manual_text_item(text)
        .map_err(|error| error.to_string())?
    {
        Some(item_id) => item_id,
        None => match repositories
            .custom_text
            .create_manual_text_and_favorite_with_tags(text, tag_ids)
        {
            Ok(_) => {
                let item_id = repositories
                    .custom_text
                    .find_manual_text_item(text)
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "新增收藏内容不存在".to_owned())?;
                return Ok((
                    QuickFavoriteSaveResult {
                        is_new: true,
                        added: true,
                    },
                    item_id,
                ));
            }
            Err(error) if is_unique_violation(&error) => repositories
                .custom_text
                .find_manual_text_item(text)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| error.to_string())?,
            Err(error) => return Err(error.to_string()),
        },
    };
    Ok((
        QuickFavoriteSaveResult {
            is_new: false,
            added: repositories
                .favorites
                .add_to_favorites_with_tags(item_id, tag_ids)
                .map_err(|error| error.to_string())?,
        },
        item_id,
    ))
}

fn favorite_captured_item(
    store: &CaptureStoreResult,
    repositories: &QuickFavoriteRepositories,
) -> Result<QuickFavoriteSaveResult, String> {
    Ok(QuickFavoriteSaveResult {
        is_new: store.is_new,
        added: repositories
            .favorites
            .add_to_favorites(store.item_id)
            .map_err(|error| error.to_string())?,
    })
}

fn is_unique_violation(error: &crate::services::custom_text::CustomTextError) -> bool {
    matches!(
        error,
        crate::services::custom_text::CustomTextError::Sql(
            rusqlite::Error::SqliteFailure(inner, _)
        ) if inner.code == rusqlite::ErrorCode::ConstraintViolation
    )
}

fn notification_message(result: QuickFavoriteSaveResult) -> (&'static str, NotificationLevel) {
    if !result.added {
        (
            "notification.quick_favorite.exists_present",
            NotificationLevel::Info,
        )
    } else if result.is_new {
        (
            "notification.quick_favorite.added",
            NotificationLevel::Success,
        )
    } else {
        (
            "notification.quick_favorite.exists_added",
            NotificationLevel::Success,
        )
    }
}

fn handle_save_result(
    from_hotkey: bool,
    result: Result<(QuickFavoriteSaveResult, ClipboardItemId), String>,
    cx: &mut App,
) {
    match result {
        Ok((result, item_id)) => {
            if from_hotkey {
                notify_favorite_saved(result, cx);
            }
            crate::windows::main::MainWindowService::refresh_saved_item(cx, item_id);
        }
        Err(error) => {
            tracing::error!("快捷收藏失败：{error}");
            let translator = crate::i18n::translator(cx);
            NotificationService::show_message_for(
                NotificationKind::Attention,
                translator.translate("notification.quick_favorite.failed_title"),
                error,
                NotificationLevel::Error,
                cx,
            );
        }
    }
}

fn notify_favorite_saved(result: QuickFavoriteSaveResult, cx: &mut App) {
    let translator = crate::i18n::translator(cx);
    let (message_key, level) = notification_message(result);
    NotificationService::show_message_for(
        NotificationKind::OperationResult,
        translator.translate("notification.quick_favorite.title"),
        translator.translate(message_key),
        level,
        cx,
    );
}

#[cfg(test)]
#[path = "quick_favorite_tests.rs"]
mod tests;
