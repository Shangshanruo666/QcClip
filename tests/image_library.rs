#![allow(dead_code)]

#[path = "../src/services/data_runtime.rs"]
pub mod data_runtime;

mod services {
    pub use crate::data_runtime;
}

#[path = "../src/services/image_library.rs"]
mod image_library;

use std::fs;

use image_library::ImageLibraryService;

#[test]
fn delete_group_removes_images_and_icon_and_preserves_other_groups() {
    let root = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let icon = source.path().join("icon.png");
    image::RgbaImage::new(2, 2).save(&icon).unwrap();
    let service = ImageLibraryService::new(root.path());
    let group = service.create_group("待删除", Some(&icon)).unwrap();
    let other = service.create_group("保留", None).unwrap();
    fs::write(root.path().join(&group.name).join("image.png"), b"image").unwrap();
    fs::write(root.path().join(&other.name).join("other.png"), b"retained").unwrap();
    let icon_path = service.group_icon_path(&group).unwrap();

    service.delete_group(&group).unwrap();

    let groups = service.load_groups().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].id, other.id);
    assert!(!root.path().join(&group.name).exists());
    assert!(!icon_path.exists());
    assert!(root.path().join(&other.name).join("other.png").exists());
    assert!(icon.exists(), "图库外的源文件应保留");
    assert!(!fs::read_dir(root.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".deleted-group-")
    }));
}

#[test]
fn delete_last_group_with_missing_directory_leaves_empty_group_list() {
    let root = tempfile::tempdir().unwrap();
    let service = ImageLibraryService::new(root.path());
    let group = service.create_group("最后一组", None).unwrap();
    fs::remove_dir(root.path().join(&group.name)).unwrap();

    service.delete_group(&group).unwrap();

    assert!(service.load_groups().unwrap().is_empty());
    assert!(service.delete_group(&group).is_err());
}

#[test]
fn delete_group_uses_persisted_name_after_rename() {
    let root = tempfile::tempdir().unwrap();
    let service = ImageLibraryService::new(root.path());
    let original = service.create_group("原名", None).unwrap();
    let renamed = service.update_group(&original, "新名", None).unwrap();
    let other = service.create_group("原名", None).unwrap();

    service.delete_group(&original).unwrap();

    assert!(!root.path().join(&renamed.name).exists());
    assert!(root.path().join(&other.name).exists());
    assert_eq!(service.load_groups().unwrap()[0].id, other.id);
}

#[cfg(windows)]
#[test]
fn delete_group_restores_images_when_group_list_cannot_be_saved() {
    use std::os::windows::fs::OpenOptionsExt;

    let root = tempfile::tempdir().unwrap();
    let service = ImageLibraryService::new(root.path());
    let group = service.create_group("保存失败", None).unwrap();
    let image_path = root.path().join(&group.name).join("image.png");
    fs::write(&image_path, b"retained").unwrap();
    let _locked = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(root.path().join("groups.json"))
        .unwrap();

    assert!(service.delete_group(&group).is_err());

    assert_eq!(fs::read(&image_path).unwrap(), b"retained");
    assert_eq!(service.load_groups().unwrap()[0].id, group.id);
}

fn usage_scores(
    service: &ImageLibraryService,
    group: &image_library::ImageLibraryGroup,
) -> Vec<(String, f64)> {
    service
        .list_images(group)
        .unwrap()
        .into_iter()
        .map(|image| (image.name, image.usage_score))
        .collect()
}

#[test]
fn record_image_use_accumulates_score_per_image() {
    let root = tempfile::tempdir().unwrap();
    let service = ImageLibraryService::new(root.path());
    let group = service.create_group("常用", None).unwrap();
    let directory = root.path().join(&group.name);
    fs::write(directory.join("a.png"), b"a").unwrap();
    fs::write(directory.join("b.png"), b"b").unwrap();

    service.record_image_use(&directory.join("a.png")).unwrap();
    service.record_image_use(&directory.join("a.png")).unwrap();
    service.record_image_use(&directory.join("b.png")).unwrap();

    let scores = usage_scores(&service, &group);
    assert_eq!(scores[0].0, "a.png");
    assert!(scores[0].1 > 1.9 && scores[0].1 <= 2.0);
    assert!(scores[1].1 > 0.9 && scores[1].1 <= 1.0);
}

#[test]
fn usage_follows_rename_and_move_and_is_cleared_on_delete() {
    let root = tempfile::tempdir().unwrap();
    let service = ImageLibraryService::new(root.path());
    let source = service.create_group("来源", None).unwrap();
    let target = service.create_group("目标", None).unwrap();
    let source_directory = root.path().join(&source.name);
    fs::write(source_directory.join("a.png"), b"a").unwrap();
    service
        .record_image_use(&source_directory.join("a.png"))
        .unwrap();

    let renamed = service
        .rename_image(&source, &source_directory.join("a.png"), "c")
        .unwrap();
    assert!(usage_scores(&service, &source)[0].1 > 0.9);

    service.move_image(&source, &target, &renamed).unwrap();
    let moved = usage_scores(&service, &target);
    assert_eq!(moved[0].0, "c.png");
    assert!(moved[0].1 > 0.9);

    let target_path = root.path().join(&target.name).join("c.png");
    service.delete_image(&target, &target_path).unwrap();
    fs::write(&target_path, b"new").unwrap();
    assert_eq!(usage_scores(&service, &target)[0].1, 0.0);
}

#[test]
fn record_image_use_rejects_paths_outside_groups() {
    let root = tempfile::tempdir().unwrap();
    let service = ImageLibraryService::new(root.path());
    service.create_group("组", None).unwrap();
    let outside = root.path().join("outside.png");
    fs::write(&outside, b"x").unwrap();
    assert!(service.record_image_use(&outside).is_err());
}
