use tempfile::TempDir;

use super::*;
use crate::services::{database::Database, storage::RawBlobStore};

impl FavoriteSession {
    fn capture(
        &self,
        context: &TestContext,
        captured: Option<&CapturedClipboard>,
    ) -> Result<OpenFavorite, String> {
        self.prepare(
            &context.capture_service,
            || Ok(captured.cloned()),
            true,
            &context.repositories(),
        )
    }
}

struct TestContext {
    _directory: TempDir,
    database: Database,
    custom_text_service: CustomTextService,
    favorite_repository: FavoriteRepository,
    tag_repository: TagRepository,
    capture_service: crate::services::capture::CaptureService,
}

impl TestContext {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("无法创建测试目录");
        let database =
            Database::open(directory.path().join("test.sqlite3")).expect("无法创建测试数据库");
        let blob_store = RawBlobStore::new(directory.path()).expect("无法创建 blob 存储");
        let custom_text_service = CustomTextService::new(database.clone(), blob_store.clone());
        let favorite_repository = FavoriteRepository::new(database.clone());
        let tag_repository = TagRepository::new(database.clone());
        let capture_service =
            crate::services::capture::CaptureService::new(database.clone(), blob_store);
        Self {
            _directory: directory,
            database,
            custom_text_service,
            favorite_repository,
            tag_repository,
            capture_service,
        }
    }

    fn capture(&self, text: &str) -> CaptureStoreResult {
        self.capture_service
            .store_captured_clipboard_with_status(
                &crate::services::capture::mock::MockClipboardSource::text(text).capture(),
            )
            .expect("无法保存测试捕获")
    }

    fn favorite_count(&self) -> i64 {
        self.count("favorites")
    }

    fn preview_text(&self, item_id: ClipboardItemId) -> String {
        self.database
            .connect()
            .expect("无法连接测试数据库")
            .query_row(
                "SELECT preview_text FROM clipboard_items WHERE id = ?1",
                [item_id],
                |row| row.get(0),
            )
            .expect("无法查询预览")
    }

    fn repositories(&self) -> QuickFavoriteRepositories {
        QuickFavoriteRepositories {
            custom_text: self.custom_text_service.clone(),
            favorites: self.favorite_repository.clone(),
            tags: self.tag_repository.clone(),
        }
    }

    fn persist(&self, text: &str, tag_ids: &[i64]) -> Result<QuickFavoriteSaveResult, String> {
        persist_manual_favorite(text, tag_ids, &self.repositories()).map(|(result, _)| result)
    }

    fn count(&self, table: &str) -> i64 {
        self.database
            .connect()
            .expect("无法连接测试数据库")
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("无法统计表行数")
    }
}

#[test]
fn recaptured_text_editor_selects_existing_tags_of_the_bound_item() {
    let context = TestContext::new();
    let captured = windows_capture(false);
    let original = context
        .capture_service
        .store_captured_clipboard_with_status(&captured)
        .unwrap();
    let alpha = context.tag_repository.create("Alpha", "#1677FF").unwrap();
    let beta = context.tag_repository.create("Beta", "#00FF00").unwrap();
    let gamma = context.tag_repository.create("Gamma", "#FF0000").unwrap();
    context
        .favorite_repository
        .add_to_favorites_with_tags(original.item_id, &[alpha.id, beta.id])
        .unwrap();

    let OpenFavorite::Editor {
        bound: Some(bound),
        tags,
        selected_tag_ids,
        ..
    } = FavoriteSession::new()
        .capture(&context, Some(&captured))
        .unwrap()
    else {
        panic!("recaptured text must open the bound editor")
    };

    assert_eq!(bound.item_id, original.item_id);
    assert_eq!(selected_tag_ids, vec![alpha.id, beta.id]);
    assert!(tags.iter().any(|tag| tag.id == gamma.id));
    assert_eq!(context.favorite_count(), 1);
}

#[test]
fn duplicate_manual_favorite_save_returns_stable_item_id_without_history() {
    let context = TestContext::new();
    let session = FavoriteSession::new();
    let (_, original_id) = session
        .save(None, "manual", &[], &context.repositories())
        .unwrap();
    let item_id = context
        .custom_text_service
        .find_manual_text_item("manual")
        .unwrap()
        .unwrap();
    assert_eq!(original_id, item_id);
    context
        .database
        .connect()
        .unwrap()
        .execute(
            "DELETE FROM clipboard_history WHERE item_id = ?1",
            [item_id],
        )
        .unwrap();

    let (result, saved_id) = session
        .save(None, "manual", &[], &context.repositories())
        .unwrap();

    assert_eq!(saved_id, item_id);
    assert!(!result.added);
    assert_eq!(context.count("clipboard_history"), 0);
    assert_eq!(context.favorite_count(), 1);
}

#[test]
fn duplicate_text_reuses_the_existing_item_and_merges_new_tags() {
    let context = TestContext::new();
    let history_id = context
        .custom_text_service
        .create_manual_text("想法")
        .expect("无法创建手动文本");
    let item_id = context
        .custom_text_service
        .find_manual_text_item("想法")
        .expect("无法查询手动文本")
        .expect("手动文本不存在");
    let existing_tag = context
        .tag_repository
        .create("工作", "#1677FF")
        .expect("无法创建已有标签");
    let new_tag = context
        .tag_repository
        .create("开发", "#00FF00")
        .expect("无法创建新标签");
    context
        .tag_repository
        .add_to_item(item_id, existing_tag.id)
        .expect("无法添加已有标签");

    let outcome = context
        .persist("想法", &[new_tag.id])
        .expect("快捷收藏失败");

    assert_eq!(
        outcome,
        QuickFavoriteSaveResult {
            is_new: false,
            added: true
        }
    );
    assert_eq!(context.count("clipboard_items"), 1);
    assert_eq!(context.count("clipboard_history"), 1);
    assert_eq!(context.count("favorites"), 1);
    assert_eq!(
        context
            .tag_repository
            .item_tag_ids(item_id)
            .expect("无法查询标签"),
        vec![existing_tag.id, new_tag.id]
    );
    assert!(
        context
            .favorite_repository
            .favorite_id_for_item(history_id)
            .expect("无法查询收藏关系")
            .is_some()
    );
}

#[test]
fn duplicate_text_already_in_favorites_is_idempotent() {
    let context = TestContext::new();
    let first = context.persist("想法", &[]).expect("首次快捷收藏失败");
    assert_eq!(
        first,
        QuickFavoriteSaveResult {
            is_new: true,
            added: true
        }
    );

    let sort_order_before: i64 = context
        .database
        .connect()
        .expect("无法连接测试数据库")
        .query_row("SELECT sort_order FROM favorites LIMIT 1", [], |row| {
            row.get(0)
        })
        .expect("无法查询收藏顺序");
    let second = context.persist("想法", &[]).expect("重复快捷收藏失败");

    assert_eq!(
        second,
        QuickFavoriteSaveResult {
            is_new: false,
            added: false
        }
    );
    assert_eq!(context.count("clipboard_items"), 1);
    assert_eq!(context.count("clipboard_history"), 1);
    assert_eq!(context.count("favorites"), 1);
    let sort_order_after: i64 = context
        .database
        .connect()
        .expect("无法连接测试数据库")
        .query_row("SELECT sort_order FROM favorites LIMIT 1", [], |row| {
            row.get(0)
        })
        .expect("无法查询收藏顺序");
    assert_eq!(sort_order_after, sort_order_before);
}

#[test]
fn favorite_save_rolls_back_when_a_selected_tag_was_deleted() {
    for edited_text in [None, Some("原始文本"), Some("修改后的文本")] {
        let context = TestContext::new();
        let history_id = context
            .custom_text_service
            .create_manual_text("原始文本")
            .unwrap();
        let item_id = context
            .custom_text_service
            .find_manual_text_item("原始文本")
            .unwrap()
            .unwrap();
        let existing = context.tag_repository.create("已有", "#1677FF").unwrap();
        let valid = context
            .tag_repository
            .create("本次有效", "#1677FF")
            .unwrap();
        let deleted = context.tag_repository.create("已删除", "#1677FF").unwrap();
        context
            .tag_repository
            .add_to_item(item_id, existing.id)
            .unwrap();
        context.tag_repository.delete_many(&[deleted.id]).unwrap();

        let result = match edited_text {
            None => context.persist("原始文本", &[valid.id, deleted.id]),
            Some(text) => persist_bound_favorite(
                BoundFavoriteItem {
                    item_id,
                    history_id,
                    original_text: "原始文本".into(),
                    is_new: false,
                },
                text,
                &[valid.id, deleted.id],
                &context.repositories(),
            ),
        };

        let error = result.expect_err("deleted tag must reject the save");
        assert!(
            error.contains("FOREIGN KEY constraint failed"),
            "unexpected save failure: {error}"
        );
        assert_eq!(
            context.tag_repository.item_tag_ids(item_id).unwrap(),
            vec![existing.id],
            "failed save must roll back all new tags: {edited_text:?}"
        );
        assert_eq!(context.preview_text(item_id), "原始文本");
        assert_eq!(context.count("clipboard_items"), 1);
        assert_eq!(context.count("clipboard_history"), 1);
        assert_eq!(context.count("clipboard_formats"), 1);
        assert_eq!(context.favorite_count(), 0);
    }
}

#[test]
fn favorite_save_rolls_back_when_favorite_insert_fails() {
    let original = "原始文本".repeat(8192);
    for edited_text in [None, Some(original.as_str()), Some("修改后的文本")] {
        let context = TestContext::new();
        let history_id = context
            .custom_text_service
            .create_manual_text(&original)
            .unwrap();
        let item_id = context
            .custom_text_service
            .find_manual_text_item(&original)
            .unwrap()
            .unwrap();
        let existing = context.tag_repository.create("已有", "#1677FF").unwrap();
        let new_tag = context
            .tag_repository
            .create("本次新增", "#1677FF")
            .unwrap();
        context
            .tag_repository
            .add_to_item(item_id, existing.id)
            .unwrap();
        let connection = context.database.connect().unwrap();
        let original_blob: String = connection
            .query_row("SELECT relative_path FROM binary_blobs", [], |row| {
                row.get(0)
            })
            .unwrap();
        let original_blob = context._directory.path().join(original_blob);
        let blob_before = std::fs::read(&original_blob).unwrap();
        let activity_before: (i64, bool) = connection
            .query_row(
                "SELECT item_id, favorite FROM sync_local_activity WHERE slot = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        connection.execute_batch("CREATE TRIGGER reject_favorite BEFORE INSERT ON favorites BEGIN SELECT RAISE(ABORT, 'temporary favorite failure'); END;").unwrap();
        drop(connection);
        let preview_before = context.preview_text(item_id);

        let result = match edited_text {
            None => context.persist(&original, &[new_tag.id]),
            Some(text) => persist_bound_favorite(
                BoundFavoriteItem {
                    item_id,
                    history_id,
                    original_text: original.clone(),
                    is_new: false,
                },
                text,
                &[new_tag.id],
                &context.repositories(),
            ),
        };

        let error = result.expect_err("favorite insertion must reject the save");
        assert!(
            error.contains("temporary favorite failure"),
            "unexpected save failure: {error}"
        );
        assert_eq!(
            context.tag_repository.item_tag_ids(item_id).unwrap(),
            vec![existing.id],
            "failed favorite must roll back new tags"
        );
        assert_eq!(context.preview_text(item_id), preview_before);
        assert_eq!(context.count("clipboard_items"), 1);
        assert_eq!(context.count("clipboard_history"), 1);
        assert_eq!(context.count("clipboard_formats"), 1);
        assert_eq!(context.favorite_count(), 0);
        assert_eq!(
            std::fs::read(original_blob).expect("rollback must keep the original external blob"),
            blob_before
        );
        let activity_after: (i64, bool) = context
            .database
            .connect()
            .unwrap()
            .query_row(
                "SELECT item_id, favorite FROM sync_local_activity WHERE slot = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(activity_after, activity_before);
    }
}

#[test]
fn trailing_whitespace_is_a_different_text() {
    let context = TestContext::new();
    let first = context.persist("想法", &[]).expect("首次快捷收藏失败");
    let second = context.persist("想法 ", &[]).expect("空白差异收藏失败");

    let created = QuickFavoriteSaveResult {
        is_new: true,
        added: true,
    };
    assert_eq!(first, created);
    assert_eq!(second, created);
    assert_eq!(context.count("clipboard_items"), 2);
    assert_eq!(context.count("clipboard_history"), 2);
    assert_eq!(context.count("favorites"), 2);
}

#[test]
fn empty_text_still_creates_a_favorite() {
    let context = TestContext::new();
    let outcome = context.persist("", &[]).expect("空内容收藏失败");

    assert_eq!(
        outcome,
        QuickFavoriteSaveResult {
            is_new: true,
            added: true
        }
    );
    assert_eq!(context.count("clipboard_items"), 1);
    assert_eq!(context.count("favorites"), 1);
}

#[test]
fn bound_item_edit_preserves_existing_tags_and_adds_new_ones() {
    let context = TestContext::new();
    let store = context.capture("原始文本");
    let bound = BoundFavoriteItem {
        item_id: store.item_id,
        history_id: store.history_id,
        original_text: "原始文本".to_owned(),
        is_new: true,
    };
    let first_tag = context
        .tag_repository
        .create("工作", "#1677FF")
        .expect("无法创建第一个标签");
    let second_tag = context
        .tag_repository
        .create("开发", "#00FF00")
        .expect("无法创建第二个标签");

    let created = persist_bound_favorite(
        bound.clone(),
        "原始文本",
        &[first_tag.id],
        &context.repositories(),
    )
    .expect("无法收藏绑定内容");
    assert_eq!(
        created,
        QuickFavoriteSaveResult {
            is_new: true,
            added: true
        }
    );

    let edited = persist_bound_favorite(
        BoundFavoriteItem {
            is_new: false,
            ..bound
        },
        "修改后的文本",
        &[second_tag.id],
        &context.repositories(),
    )
    .expect("无法更新绑定内容");
    assert_eq!(
        edited,
        QuickFavoriteSaveResult {
            is_new: false,
            added: false
        }
    );
    assert_eq!(context.favorite_count(), 1);
    assert_eq!(context.count("clipboard_items"), 1);
    assert_eq!(
        context
            .tag_repository
            .item_tag_ids(store.item_id)
            .expect("无法查询标签"),
        {
            let mut expected = vec![first_tag.id, second_tag.id];
            expected.sort_unstable();
            expected
        }
    );
    assert_eq!(context.preview_text(store.item_id), "修改后的文本");
}

#[test]
fn captured_item_without_editor_is_favorited_directly() {
    let context = TestContext::new();
    let store = context.capture("图片占位");

    let outcome =
        favorite_captured_item(&store, &context.repositories()).expect("无法直接收藏捕获条目");

    assert_eq!(
        outcome,
        QuickFavoriteSaveResult {
            is_new: true,
            added: true
        }
    );
    assert_eq!(context.favorite_count(), 1);
    assert_eq!(context.count("clipboard_items"), 1);
    assert_eq!(context.count("clipboard_history"), 1);
}

#[test]
fn bound_item_edit_hash_conflict_fails_without_favoriting() {
    let context = TestContext::new();
    let history_id = context
        .custom_text_service
        .create_manual_text("原始文本")
        .expect("无法创建待编辑文本");
    let item_id = context
        .custom_text_service
        .find_manual_text_item("原始文本")
        .expect("无法查询待编辑文本")
        .expect("待编辑文本不存在");
    context
        .custom_text_service
        .create_manual_text("已存在")
        .expect("无法创建冲突文本");
    let bound = BoundFavoriteItem {
        item_id,
        history_id,
        original_text: "原始文本".to_owned(),
        is_new: false,
    };

    let result = persist_bound_favorite(bound, "已存在", &[], &context.repositories());

    assert!(result.is_err());
    assert_eq!(context.favorite_count(), 0);
}

#[test]
fn notification_messages_follow_the_three_outcomes() {
    assert_eq!(
        notification_message(QuickFavoriteSaveResult {
            is_new: true,
            added: true
        }),
        (
            "notification.quick_favorite.added",
            NotificationLevel::Success
        )
    );
    assert_eq!(
        notification_message(QuickFavoriteSaveResult {
            is_new: false,
            added: true
        }),
        (
            "notification.quick_favorite.exists_added",
            NotificationLevel::Success
        )
    );
    assert_eq!(
        notification_message(QuickFavoriteSaveResult {
            is_new: false,
            added: false
        }),
        (
            "notification.quick_favorite.exists_present",
            NotificationLevel::Info
        )
    );
}
fn windows_capture(image: bool) -> CapturedClipboard {
    use crate::domain::capture::{FormatKind, RawFormat, SourceApplication};
    let formats = if image {
        vec![RawFormat::captured(
            "windows",
            "PNG",
            "PNG",
            FormatKind::Image,
            vec![137, 80, 78, 71, 13, 10, 26, 10],
            0,
            true,
        )]
    } else {
        vec![
            RawFormat::captured(
                "windows",
                "CF_UNICODETEXT",
                "Unicode text",
                FormatKind::Text,
                "富文本\0"
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect(),
                0,
                true,
            ),
            RawFormat::captured(
                "windows",
                "HTML Format",
                "HTML",
                FormatKind::Html,
                b"<b>rich</b>".to_vec(),
                1,
                true,
            ),
            RawFormat::captured(
                "windows",
                "Rich Text Format",
                "RTF",
                FormatKind::Rtf,
                br"{\rtf1 rich}".to_vec(),
                2,
                true,
            ),
        ]
    };
    CapturedClipboard {
        primary_format_identity: Some((
            formats[0].format_namespace.clone(),
            formats[0].format_key.clone(),
        )),
        formats,
        preview_candidate: None,
        source_application: SourceApplication::default(),
    }
}

#[test]
fn failed_unicode_text_is_rejected_before_any_capture_is_saved() {
    use crate::domain::capture::{FormatKind, RawFormat};

    let context = TestContext::new();
    let mut captured = windows_capture(false);
    captured.formats[0] = RawFormat::failed(
        "windows",
        "CF_UNICODETEXT",
        "Unicode text",
        FormatKind::Text,
        0,
        "clipboard_read_failed",
        "clipboard busy",
    );

    let failed = FavoriteSession::new().capture(&context, Some(&captured));
    assert!(
        matches!(failed, Err(ref message) if message.contains("clipboard busy")),
        "text read failure must reach the UI: {failed:?}"
    );
    for table in [
        "clipboard_items",
        "clipboard_formats",
        "clipboard_history",
        "favorites",
        "sync_local_activity",
    ] {
        assert_eq!(
            context.count(table),
            0,
            "failed selection must not write {table}"
        );
    }

    let OpenFavorite::Editor { text, .. } = FavoriteSession::new()
        .capture(&context, Some(&windows_capture(false)))
        .unwrap()
    else {
        panic!("successful retry must store the complete capture")
    };
    assert_eq!(text, "富文本");
    assert_eq!(context.count("clipboard_items"), 1);
    assert_eq!(context.count("clipboard_formats"), 3);
    assert_eq!(context.favorite_count(), 0);
}

#[test]
fn capture_without_unicode_text_is_favorited_as_non_text() {
    use crate::domain::capture::FormatKind;

    let context = TestContext::new();
    let mut captured = windows_capture(false);
    captured
        .formats
        .retain(|format| format.format_kind == FormatKind::Html);
    captured.primary_format_identity = Some(("windows".into(), "HTML Format".into()));

    let OpenFavorite::Saved { result, .. } = FavoriteSession::new()
        .capture(&context, Some(&captured))
        .unwrap()
    else {
        panic!("capture without Unicode text must be favorited directly")
    };
    assert!(result.added);
    assert_eq!(context.count("clipboard_items"), 1);
    assert_eq!(context.count("clipboard_formats"), 1);
    assert_eq!(context.favorite_count(), 1);
}

#[test]
fn failed_rich_capture_is_not_a_manual_favorite_and_retry_preserves_formats() {
    let context = TestContext::new();
    let captured = windows_capture(false);
    let connection = context.database.connect().unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER reject_capture BEFORE INSERT ON clipboard_items
         BEGIN SELECT RAISE(ABORT, 'temporary capture failure'); END;",
        )
        .unwrap();
    let failed = FavoriteSession::new().capture(&context, Some(&captured));
    assert!(
        matches!(failed, Err(ref message) if message.contains("temporary capture failure")),
        "storage error must reach the UI: {failed:?}"
    );
    assert_eq!(context.favorite_count(), 0);
    connection
        .execute_batch("DROP TRIGGER reject_capture")
        .unwrap();
    let OpenFavorite::Editor {
        text,
        bound: Some(bound),
        ..
    } = FavoriteSession::new()
        .capture(&context, Some(&captured))
        .unwrap()
    else {
        panic!("retry must produce the captured item")
    };
    assert_eq!(text, "富文本");
    let item_id = bound.item_id;
    let session = FavoriteSession::new();
    assert!(
        session
            .save(Some(bound.clone()), "编辑后", &[], &context.repositories())
            .unwrap()
            .0
            .added
    );
    assert!(
        !session
            .save(Some(bound), "编辑后", &[], &context.repositories())
            .unwrap()
            .0
            .added
    );
    for format in &captured.formats {
        let actual: Vec<u8> = connection
            .query_row(
                "SELECT inline_data FROM clipboard_formats WHERE item_id = ?1 AND format_key = ?2",
                rusqlite::params![item_id, format.format_key],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(actual, *format.bytes.as_ref().unwrap());
    }
    assert_eq!(context.count("clipboard_items"), 1);
    assert_eq!(context.favorite_count(), 1);
}

#[test]
fn failed_image_capture_is_not_reported_as_no_selection() {
    let context = TestContext::new();
    context
        .database
        .connect()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER reject_capture BEFORE INSERT ON clipboard_items
         BEGIN SELECT RAISE(ABORT, 'temporary capture failure'); END;",
        )
        .unwrap();
    let result = FavoriteSession::new().capture(&context, Some(&windows_capture(true)));
    assert!(
        matches!(result, Err(ref message) if message.contains("temporary capture failure")),
        "{result:?}"
    );
}

#[test]
fn retired_editor_cannot_save_a_bound_item_into_replacement_database() {
    let old = TestContext::new();
    let new = TestContext::new();
    let old_item = old.capture("old");
    let new_item = new.capture("new");
    assert_eq!(old_item.item_id, new_item.item_id);
    let session = FavoriteSession::new();
    session.retire();
    let bound = BoundFavoriteItem {
        item_id: old_item.item_id,
        history_id: old_item.history_id,
        original_text: "old".into(),
        is_new: true,
    };
    let result = session.save(Some(bound), "overwrite", &[], &new.repositories());
    assert!(
        result.is_err(),
        "retired editor must not reuse the ID in the replacement database"
    );
    assert_eq!(new.preview_text(new_item.item_id), "new");
    assert_eq!(old.favorite_count(), 0);
    assert_eq!(new.favorite_count(), 0);
}

#[test]
fn queued_capture_cannot_write_to_retired_data_source() {
    let context = TestContext::new();
    let session = FavoriteSession::new();
    let queued = session.clone();
    session.retire();
    let result = queued.capture(&context, Some(&windows_capture(false)));
    assert!(result.is_err());
    assert_eq!(context.count("clipboard_items"), 0);
}

#[test]
fn replacement_routes_manual_capture_and_external_blobs_to_new_sources() {
    let old = TestContext::new();
    let new = TestContext::new();
    let mut service = QuickFavoriteService {
        repositories: old.repositories(),
        capture_service: old.capture_service.clone(),
        clipboard_write_suppressor: ClipboardWriteSuppressor::default(),
        editor_window: None,
        opening: false,
        session: FavoriteSession::new(),
    };
    service.session.retire();
    service.replace_sources(
        new.database.clone(),
        RawBlobStore::new(new._directory.path()).unwrap(),
    );
    service.session = FavoriteSession::new();
    let tag = new.tag_repository.create("new source", "#1677FF").unwrap();
    service
        .session
        .save(None, "manual", &[tag.id], &service.repositories)
        .unwrap();
    let mut captured = windows_capture(false);
    captured.formats[1].bytes = Some(vec![b'x'; 80 * 1024]);
    let OpenFavorite::Editor {
        text,
        bound: Some(bound),
        ..
    } = service
        .session
        .prepare(
            &service.capture_service,
            || Ok(Some(captured.clone())),
            true,
            &service.repositories,
        )
        .unwrap()
    else {
        panic!("capture must be stored")
    };
    assert_eq!(text, "富文本");
    let item_id = bound.item_id;
    service
        .session
        .save(Some(bound), "edited", &[tag.id], &service.repositories)
        .unwrap();
    assert_eq!(old.count("clipboard_items"), 0);
    assert_eq!(old.count("binary_blobs"), 0);
    assert_eq!(new.count("clipboard_items"), 2);
    assert_eq!(new.favorite_count(), 2);
    assert_eq!(
        new.tag_repository.item_tag_ids(item_id).unwrap(),
        vec![tag.id]
    );
    let connection = new.database.connect().unwrap();
    let relative_path: String = connection
        .query_row("SELECT relative_path FROM binary_blobs", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(new._directory.path().join(&relative_path).is_file());
    assert!(!old._directory.path().join(&relative_path).exists());
}

#[test]
fn only_absent_selection_is_empty_and_non_text_capture_stays_bound() {
    let context = TestContext::new();
    assert!(matches!(
        FavoriteSession::new().capture(&context, None),
        Ok(OpenFavorite::Editor { bound: None, .. })
    ));
    let mut failed_formats = windows_capture(false);
    failed_formats.formats.clear();
    assert!(matches!(
        FavoriteSession::new().capture(&context, Some(&failed_formats)),
        Err(_)
    ));
    for kind in [
        crate::domain::capture::FormatKind::Image,
        crate::domain::capture::FormatKind::FileList,
    ] {
        let mut captured = windows_capture(true);
        if kind == crate::domain::capture::FormatKind::FileList {
            captured.formats[0] = crate::domain::capture::RawFormat::captured(
                "mime",
                "text/uri-list",
                "Files",
                kind,
                b"file:///C:/example.txt".to_vec(),
                0,
                true,
            );
            captured.primary_format_identity = Some(("mime".into(), "text/uri-list".into()));
        }
        let OpenFavorite::Saved { result, .. } = FavoriteSession::new()
            .capture(&context, Some(&captured))
            .unwrap()
        else {
            panic!("non-text capture must be favorited directly")
        };
        assert!(result.added);
        let OpenFavorite::Saved { result, .. } = FavoriteSession::new()
            .capture(&context, Some(&captured))
            .unwrap()
        else {
            panic!("duplicate non-text capture must stay bound")
        };
        assert!(!result.added);
    }
    assert_eq!(context.count("clipboard_items"), 2);
    assert_eq!(context.favorite_count(), 2);
}

#[test]
fn failed_switch_resumes_with_a_fresh_session_only() {
    let context = TestContext::new();
    let old = FavoriteSession::new();
    old.retire();
    let resumed = FavoriteSession::new();
    resumed
        .save(None, "after failed switch", &[], &context.repositories())
        .unwrap();
    assert!(
        old.save(None, "stale", &[], &context.repositories())
            .is_err()
    );
    assert_eq!(context.count("clipboard_items"), 1);
    assert_eq!(context.favorite_count(), 1);
}

#[test]
fn runtime_switch_waits_for_active_session_and_rejects_queued_work() {
    // Isolate the process-global runtime so parallel database tests remain independent.
    const CHILD: &str = "QCCLIP_TEST_FAVORITE_RUNTIME";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "services::quick_favorite::tests::runtime_switch_waits_for_active_session_and_rejects_queued_work", "--test-threads=1"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    use std::{sync::mpsc, time::Duration};
    let context = TestContext::new();
    data_runtime::initialize();
    let old = FavoriteSession::new();
    let active = old.begin_operation().unwrap();
    old.retire();
    let (started_tx, started_rx) = mpsc::channel();
    let (switched_tx, switched_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let switch = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        let _switch = data_runtime::begin_switch();
        switched_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    });
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(
        switched_rx.recv_timeout(Duration::from_millis(50)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    drop(active);
    switched_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let current = FavoriteSession::new();
    let captured = windows_capture(false);
    assert!(current.capture(&context, Some(&captured)).is_err());
    release_tx.send(()).unwrap();
    switch.join().unwrap();
    assert!(old.capture(&context, Some(&captured)).is_err());
    assert_eq!(context.count("clipboard_items"), 0);
    assert!(matches!(
        current.capture(&context, Some(&captured)),
        Ok(OpenFavorite::Editor { .. })
    ));
    assert_eq!(context.count("clipboard_items"), 1);
}
