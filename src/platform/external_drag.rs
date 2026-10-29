use std::{
    collections::HashSet,
    mem::ManuallyDrop,
    ptr,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use gpui::{AnyWindowHandle, App, BorrowAppContext as _, Global};
use windows::{
    Win32::{
        Foundation::{
            DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, GlobalFree, HGLOBAL,
        },
        System::{
            Com::{
                DATADIR_GET, DVASPECT_CONTENT, FORMATETC, IAdviseSink, IDataObject,
                IDataObject_Impl, IEnumFORMATETC, IEnumFORMATETC_Impl, IEnumSTATDATA, STGMEDIUM,
                STGMEDIUM_0, TYMED_HGLOBAL,
            },
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
            Ole::{
                CF_DIB, CF_DIBV5, CF_HDROP, CF_TEXT, CF_UNICODETEXT, DROPEFFECT, DROPEFFECT_COPY,
                DoDragDrop, IDropSource, IDropSource_Impl, OleInitialize,
            },
            SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS},
        },
    },
    core::{BOOL, Error, PCWSTR, Ref, Result as WindowsResult, implement},
};

use crate::platform::clipboard::ClipboardPayload;
use crate::platform::drag_preview::{DragPreviewService, NativeDragPreviewTarget};
use crate::platform::image_temp_file::{ImageTempFile, create_image_temp_file, hdrop_bytes};
use crate::platform::input::pointer_snapshot;
use crate::platform::window::{PhysicalClientBounds, physical_client_bounds};

const E_NOTIMPL: windows::core::HRESULT = windows::core::HRESULT(0x8000_4001u32 as i32);
const E_INVALIDARG: windows::core::HRESULT = windows::core::HRESULT(0x8007_0057u32 as i32);
const DV_E_FORMATETC: windows::core::HRESULT = windows::core::HRESULT(0x8004_0064u32 as i32);
const E_FAIL: windows::core::HRESULT = windows::core::HRESULT(0x8000_4005u32 as i32);
const S_OK: windows::core::HRESULT = windows::core::HRESULT(0);
const S_FALSE: windows::core::HRESULT = windows::core::HRESULT(1);
const IMAGE_TEMP_FILE_RETENTION: Duration = Duration::from_secs(30);
static OLE_INITIALIZED: OnceLock<Result<(), String>> = OnceLock::new();

#[derive(Default)]
pub(crate) struct ExternalDragService {
    session: Option<ExternalDragSession>,
    retained_image_files: Vec<RetainedImageFile>,
}

impl Global for ExternalDragService {}

struct ExternalDragSession {
    source_window: AnyWindowHandle,
    payloads: Vec<ClipboardPayload>,
    /// 多选合并时 CF_HDROP 引用的临时图片，拖拽结束后转入保留池。
    image_files: Vec<ImageTempFile>,
    preview_target: Option<NativeDragPreviewTarget>,
}

pub(crate) struct ExternalDragRequest {
    session: ExternalDragSession,
    source_bounds: PhysicalClientBounds,
}

pub(crate) struct ExternalDragCompletion {
    session: ExternalDragSession,
    result: Result<DragDropResult, String>,
}

impl ExternalDragService {
    pub(crate) fn install(cx: &mut App) {
        cx.set_global(Self::default());
        cx.on_window_closed(|cx, window_id| {
            cx.update_global::<Self, _>(|service, _| {
                if service
                    .session
                    .as_ref()
                    .is_some_and(|session| session.source_window.window_id() == window_id)
                {
                    service.session = None;
                }
            });
        })
        .detach();
    }

    pub(crate) fn begin(
        source_window: AnyWindowHandle,
        payloads: Vec<ClipboardPayload>,
        cx: &mut App,
    ) -> Result<(), String> {
        Self::begin_with_files(source_window, payloads, Vec::new(), cx)
    }

    pub(crate) fn begin_with_files(
        source_window: AnyWindowHandle,
        payloads: Vec<ClipboardPayload>,
        image_files: Vec<ImageTempFile>,
        cx: &mut App,
    ) -> Result<(), String> {
        if payloads.is_empty() {
            return Err("没有可用于外部拖拽的剪贴板格式".to_owned());
        }
        let preview_target = DragPreviewService::native_follow_target(cx);
        cx.update_global::<Self, _>(|service, _| {
            service.session = Some(ExternalDragSession {
                source_window,
                payloads,
                image_files,
                preview_target,
            });
        });
        Ok(())
    }

    pub(crate) fn prepare_poll(cx: &mut App) -> Option<ExternalDragRequest> {
        let now = Instant::now();
        cx.update_global::<Self, _>(|service, _| {
            service
                .retained_image_files
                .retain(|retained| retained.expires_at > now);
        });

        let Some(source_window) = cx.try_global::<Self>().and_then(|service| {
            service
                .session
                .as_ref()
                .map(|session| session.source_window)
        }) else {
            return None;
        };

        let Some(pointer) = pointer_snapshot() else {
            return None;
        };
        if !pointer.buttons.left {
            Self::cancel(cx);
            return None;
        }

        let bounds = match source_window.update(cx, |_, window, _| physical_client_bounds(window)) {
            Ok(Ok(bounds)) => bounds,
            Ok(Err(error)) => {
                tracing::debug!(%error, "读取外部拖拽源窗口边界失败，已取消拖拽");
                Self::cancel(cx);
                return None;
            }
            Err(error) => {
                tracing::debug!(%error, "外部拖拽源窗口已不可用，已取消拖拽");
                Self::cancel(cx);
                return None;
            }
        };
        let x = pointer.position.x as i32;
        let y = pointer.position.y as i32;
        if x >= bounds.left && x < bounds.right && y >= bounds.top && y < bounds.bottom {
            return None;
        }

        let session = cx.update_global::<Self, _>(|service, _| service.session.take());
        session.map(|session| ExternalDragRequest {
            session,
            source_bounds: bounds,
        })
    }

    /// 必须在 GPUI 的 App 借用之外运行，DoDragDrop 会同步泵送 Windows 消息。
    pub(crate) fn execute(request: ExternalDragRequest) -> ExternalDragCompletion {
        let result = start_drag(
            &request.session.payloads,
            request.session.preview_target,
            request.source_bounds,
        );
        ExternalDragCompletion {
            session: request.session,
            result,
        }
    }

    pub(crate) fn finish(completion: ExternalDragCompletion, cx: &mut App) {
        let ExternalDragCompletion { session, result } = completion;
        match result {
            Ok(DragDropResult {
                outcome: DragDropOutcome::ReenteredSourceWindow,
                image_temp_file: _,
            }) => {
                cx.update_global::<Self, _>(|service, _| service.session = Some(session));
                DragPreviewService::refresh(cx);
            }
            Ok(DragDropResult {
                outcome: DragDropOutcome::Finished,
                image_temp_file,
            }) => {
                let expires_at = Instant::now() + IMAGE_TEMP_FILE_RETENTION;
                cx.update_global::<Self, _>(|service, _| {
                    service.retained_image_files.extend(
                        image_temp_file
                            .into_iter()
                            .chain(session.image_files)
                            .map(|file| RetainedImageFile {
                                expires_at,
                                _file: file,
                            }),
                    );
                });
                DragPreviewService::finish(cx);
            }
            Err(error) => {
                tracing::error!("外部拖拽失败：{error}");
                DragPreviewService::finish(cx);
            }
        }
    }

    pub(crate) fn cancel(cx: &mut App) {
        cx.update_global::<Self, _>(|service, _| service.session = None);
    }
}

#[derive(Clone, Debug)]
struct DragFormat {
    format_id: u16,
    bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DragDropOutcome {
    Finished,
    ReenteredSourceWindow,
}

struct DragDropResult {
    outcome: DragDropOutcome,
    image_temp_file: Option<ImageTempFile>,
}

/// 启动一次 Windows 原生拖拽。数据由目标程序按剪贴板格式读取，模块不设置系统拖拽图像。
fn start_drag(
    payloads: &[ClipboardPayload],
    preview_target: Option<NativeDragPreviewTarget>,
    source_bounds: PhysicalClientBounds,
) -> Result<DragDropResult, String> {
    if payloads.is_empty() {
        return Err("没有可用于外部拖拽的剪贴板格式".to_owned());
    }
    initialize_ole()?;

    let image_temp_file = create_image_temp_file(payloads)?;
    let formats = build_formats(payloads, image_temp_file.as_ref())?;
    if formats.is_empty() {
        return Err("没有可用于外部拖拽的剪贴板格式".to_owned());
    }

    let data_object: IDataObject = DataObject { formats }.into();
    let reentered_source_window = Arc::new(AtomicBool::new(false));
    let drop_source: IDropSource = DropSource {
        preview_target,
        source_bounds,
        reentered_source_window: reentered_source_window.clone(),
    }
    .into();
    let mut effect = DROPEFFECT::default();

    // SAFETY: OLE 已在当前 UI 线程初始化；data_object 和 drop_source 在 DoDragDrop 返回前保持有效。
    let result = unsafe { DoDragDrop(&data_object, &drop_source, DROPEFFECT_COPY, &mut effect) };

    if result == DRAGDROP_S_DROP || result == DRAGDROP_S_CANCEL {
        let outcome = if reentered_source_window.load(Ordering::Acquire) {
            DragDropOutcome::ReenteredSourceWindow
        } else {
            DragDropOutcome::Finished
        };
        Ok(DragDropResult {
            image_temp_file: if outcome == DragDropOutcome::Finished {
                image_temp_file
            } else {
                None
            },
            outcome,
        })
    } else {
        Err(format!("外部拖拽未完成：{result:?}"))
    }
}

fn initialize_ole() -> Result<(), String> {
    OLE_INITIALIZED
        .get_or_init(|| {
            // SAFETY: 当前线程是 GPUI UI 线程，只初始化该线程的 OLE 环境。
            unsafe { OleInitialize(None) }
                .map(|_| ())
                .map_err(|error| format!("初始化 Windows OLE 失败：{error}"))
        })
        .clone()
}

struct RetainedImageFile {
    expires_at: Instant,
    _file: ImageTempFile,
}

fn build_formats(
    payloads: &[ClipboardPayload],
    image_temp_file: Option<&ImageTempFile>,
) -> Result<Vec<DragFormat>, String> {
    let mut seen = HashSet::new();
    let mut formats = payloads
        .iter()
        .filter_map(|payload| {
            let format_id = match format_id(payload) {
                Ok(format_id) => format_id,
                Err(error) => {
                    tracing::error!("跳过无法转换的外部拖拽格式：{error}");
                    return None;
                }
            };
            if !seen.insert(format_id) {
                return None;
            }
            Some(DragFormat {
                format_id,
                bytes: payload.bytes.clone(),
            })
        })
        .collect::<Vec<_>>();

    if let Some(image_temp_file) = image_temp_file {
        let format_id = CF_HDROP.0;
        if seen.insert(format_id) {
            formats.insert(
                0,
                DragFormat {
                    format_id,
                    bytes: hdrop_bytes(image_temp_file.path()),
                },
            );
        }
    }

    Ok(formats)
}

fn format_id(payload: &ClipboardPayload) -> Result<u16, String> {
    let id = match payload.format_key.as_str() {
        "CF_TEXT" => CF_TEXT.0 as u32,
        "CF_DIB" => CF_DIB.0 as u32,
        "CF_UNICODETEXT" => CF_UNICODETEXT.0 as u32,
        "CF_HDROP" => CF_HDROP.0 as u32,
        "CF_DIBV5" => CF_DIBV5.0 as u32,
        key if key.starts_with("registered:") => {
            let name = payload
                .format_name
                .encode_utf16()
                .chain([0])
                .collect::<Vec<_>>();
            // SAFETY: name 是本地有效的 NUL 结尾 UTF-16 缓冲区，仅在调用期间使用。
            unsafe {
                windows::Win32::System::DataExchange::RegisterClipboardFormatW(PCWSTR(
                    name.as_ptr(),
                ))
            }
        }
        key if key.starts_with("native:") => key
            .strip_prefix("native:")
            .and_then(|value| value.parse::<u32>().ok())
            .or(payload.native_format_id)
            .ok_or_else(|| format!("原生格式 ID 无效：{key}"))?,
        _ => payload.native_format_id.ok_or_else(|| {
            format!(
                "格式没有可用的原生格式 ID：{} ({})",
                payload.format_name, payload.format_key
            )
        })?,
    };
    if id == 0 {
        return Err(format!("无法注册剪贴板格式：{}", payload.format_name));
    }
    u16::try_from(id).map_err(|_| format!("剪贴板格式 ID 超出范围：{id}"))
}

#[implement(IDropSource)]
struct DropSource {
    preview_target: Option<NativeDragPreviewTarget>,
    source_bounds: PhysicalClientBounds,
    reentered_source_window: Arc<AtomicBool>,
}

#[allow(non_snake_case)]
impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(
        &self,
        escape_pressed: BOOL,
        key_state: MODIFIERKEYS_FLAGS,
    ) -> windows::core::HRESULT {
        if escape_pressed.as_bool() {
            DRAGDROP_S_CANCEL
        } else if (key_state & MK_LBUTTON) == MODIFIERKEYS_FLAGS(0) {
            DRAGDROP_S_DROP
        } else if pointer_is_inside(self.source_bounds) {
            self.reentered_source_window.store(true, Ordering::Release);
            DRAGDROP_S_CANCEL
        } else {
            self.follow_preview();
            S_OK
        }
    }

    fn GiveFeedback(&self, _effect: DROPEFFECT) -> windows::core::HRESULT {
        self.follow_preview();
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

impl DropSource_Impl {
    fn follow_preview(&self) {
        let Some(target) = self.preview_target else {
            return;
        };
        let Some(pointer) = pointer_snapshot() else {
            return;
        };
        move_native_preview(target, pointer.position);
    }
}

fn pointer_is_inside(bounds: PhysicalClientBounds) -> bool {
    let Some(pointer) = pointer_snapshot() else {
        return false;
    };
    let x = pointer.position.x as i32;
    let y = pointer.position.y as i32;
    x >= bounds.left && x < bounds.right && y >= bounds.top && y < bounds.bottom
}

#[cfg(target_os = "windows")]
fn move_native_preview(target: NativeDragPreviewTarget, pointer: gpui::Point<f32>) {
    use windows::Win32::{
        Foundation::HWND,
        UI::WindowsAndMessaging::{
            HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_SHOWWINDOW, SetWindowPos,
        },
    };

    let hwnd = HWND(target.hwnd as *mut _);
    // SAFETY: hwnd 来自仍存活的 GPUI 预览窗口；拖拽期间只更新其位置和客户区尺寸。
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            (pointer.x - target.cursor_offset_physical.x).round() as i32,
            (pointer.y - target.cursor_offset_physical.y).round() as i32,
            target.width_physical.round().max(1.0) as i32,
            target.height_physical.round().max(1.0) as i32,
            SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_SHOWWINDOW,
        );
    }
}

#[cfg(not(target_os = "windows"))]
fn move_native_preview(_target: NativeDragPreviewTarget, _pointer: gpui::Point<f32>) {}

#[implement(IDataObject)]
struct DataObject {
    formats: Vec<DragFormat>,
}

impl DataObject {
    fn supports(format: &FORMATETC, formats: &[DragFormat]) -> bool {
        format.dwAspect == DVASPECT_CONTENT.0
            && (format.tymed & TYMED_HGLOBAL.0 as u32) != 0
            && formats.iter().any(|item| item.format_id == format.cfFormat)
    }

    fn format_list(&self) -> Vec<FORMATETC> {
        self.formats
            .iter()
            .map(|item| FORMATETC {
                cfFormat: item.format_id,
                ptd: ptr::null_mut(),
                dwAspect: DVASPECT_CONTENT.0,
                lindex: -1,
                tymed: TYMED_HGLOBAL.0 as u32,
            })
            .collect()
    }
}

#[allow(non_snake_case)]
impl IDataObject_Impl for DataObject_Impl {
    fn GetData(&self, format: *const FORMATETC) -> WindowsResult<STGMEDIUM> {
        let Some(format) = (unsafe { format.as_ref() }) else {
            return Err(Error::new(E_INVALIDARG, ""));
        };
        if !DataObject::supports(format, &self.formats) {
            return Err(Error::new(DV_E_FORMATETC, ""));
        }
        let Some(item) = self
            .formats
            .iter()
            .find(|item| item.format_id == format.cfFormat)
        else {
            return Err(Error::new(DV_E_FORMATETC, ""));
        };
        let handle = global_memory(&item.bytes)?;
        Ok(STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 { hGlobal: handle },
            pUnkForRelease: ManuallyDrop::new(None),
        })
    }

    fn GetDataHere(&self, _format: *const FORMATETC, _medium: *mut STGMEDIUM) -> WindowsResult<()> {
        Err(Error::new(E_NOTIMPL, ""))
    }

    fn QueryGetData(&self, format: *const FORMATETC) -> windows::core::HRESULT {
        let Some(format) = (unsafe { format.as_ref() }) else {
            return E_INVALIDARG;
        };
        if DataObject::supports(format, &self.formats) {
            S_OK
        } else {
            DV_E_FORMATETC
        }
    }

    fn GetCanonicalFormatEtc(
        &self,
        _input: *const FORMATETC,
        output: *mut FORMATETC,
    ) -> windows::core::HRESULT {
        if !output.is_null() {
            // SAFETY: 调用方提供了可写的 FORMATETC 输出指针。
            unsafe { (*output).ptd = ptr::null_mut() };
        }
        E_NOTIMPL
    }

    fn SetData(
        &self,
        _format: *const FORMATETC,
        _medium: *const STGMEDIUM,
        _release: BOOL,
    ) -> WindowsResult<()> {
        Err(Error::new(E_NOTIMPL, ""))
    }

    fn EnumFormatEtc(&self, direction: u32) -> WindowsResult<IEnumFORMATETC> {
        if direction != DATADIR_GET.0 as u32 {
            return Err(Error::new(E_NOTIMPL, ""));
        }
        Ok(FormatEnumerator {
            formats: self.format_list(),
            index: Mutex::new(0),
        }
        .into())
    }

    fn DAdvise(
        &self,
        _format: *const FORMATETC,
        _flags: u32,
        _sink: Ref<'_, IAdviseSink>,
    ) -> WindowsResult<u32> {
        Err(Error::new(E_NOTIMPL, ""))
    }

    fn DUnadvise(&self, _connection: u32) -> WindowsResult<()> {
        Err(Error::new(E_NOTIMPL, ""))
    }

    fn EnumDAdvise(&self) -> WindowsResult<IEnumSTATDATA> {
        Err(Error::new(E_NOTIMPL, ""))
    }
}

#[implement(IEnumFORMATETC)]
struct FormatEnumerator {
    formats: Vec<FORMATETC>,
    index: Mutex<usize>,
}

#[allow(non_snake_case)]
impl IEnumFORMATETC_Impl for FormatEnumerator_Impl {
    fn Next(
        &self,
        count: u32,
        output: *mut FORMATETC,
        fetched: *mut u32,
    ) -> windows::core::HRESULT {
        if output.is_null() {
            return E_INVALIDARG;
        }
        let Ok(mut index) = self.index.lock() else {
            return E_FAIL;
        };
        let available = self.formats.len().saturating_sub(*index);
        let take = available.min(count as usize);
        for offset in 0..take {
            // SAFETY: output 指向调用方提供的 count 个 FORMATETC 缓冲区。
            unsafe { output.add(offset).write(self.formats[*index + offset]) };
        }
        *index += take;
        if !fetched.is_null() {
            // SAFETY: fetched 是可选的调用方输出指针。
            unsafe { fetched.write(take as u32) };
        }
        if take == count as usize {
            S_OK
        } else {
            S_FALSE
        }
    }

    fn Skip(&self, count: u32) -> WindowsResult<()> {
        let Ok(mut index) = self.index.lock() else {
            return Err(Error::new(E_FAIL, ""));
        };
        let available = self.formats.len().saturating_sub(*index);
        let skipped = available.min(count as usize);
        *index += skipped;
        if skipped == count as usize {
            Ok(())
        } else {
            Err(Error::new(S_FALSE, ""))
        }
    }

    fn Reset(&self) -> WindowsResult<()> {
        let Ok(mut index) = self.index.lock() else {
            return Err(Error::new(E_FAIL, ""));
        };
        *index = 0;
        Ok(())
    }

    fn Clone(&self) -> WindowsResult<IEnumFORMATETC> {
        let index = self.index.lock().map_err(|_| Error::new(E_FAIL, ""))?;
        Ok(FormatEnumerator {
            formats: self.formats.clone(),
            index: Mutex::new(*index),
        }
        .into())
    }
}

fn global_memory(bytes: &[u8]) -> WindowsResult<HGLOBAL> {
    let allocation_size = bytes.len().max(1);
    // SAFETY: GMEM_MOVEABLE 是 OLE HGLOBAL 数据传输要求的内存类型。
    let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, allocation_size)? };
    // SAFETY: handle 来自刚分配的全局内存，尚未交给其它组件。
    let pointer = unsafe { GlobalLock(handle) };
    if pointer.is_null() {
        // SAFETY: 锁定失败时 handle 仍由当前函数拥有。
        unsafe { GlobalFree(Some(handle)) }?;
        return Err(Error::new(
            windows::core::HRESULT(0x8007_000Eu32 as i32),
            "",
        ));
    }
    // SAFETY: pointer 指向 allocation_size 字节，复制长度不超过分配大小。
    unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), pointer.cast::<u8>(), bytes.len()) };
    // SAFETY: 释放当前线程对全局内存的锁定。
    let _ = unsafe { GlobalUnlock(handle) };
    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_clipboard_formats_use_native_ids() {
        let payload = ClipboardPayload {
            format_kind: "text".to_owned(),
            format_namespace: "windows".to_owned(),
            format_key: "CF_UNICODETEXT".to_owned(),
            format_name: "CF_UNICODETEXT".to_owned(),
            native_format_id: Some(13),
            bytes: vec![0, 0],
        };
        assert_eq!(format_id(&payload).expect("格式 ID 应有效"), 13);
    }

    #[test]
    fn duplicate_formats_are_removed_before_ole_drag() {
        let payload = ClipboardPayload {
            format_kind: "text".to_owned(),
            format_namespace: "windows".to_owned(),
            format_key: "CF_UNICODETEXT".to_owned(),
            format_name: "CF_UNICODETEXT".to_owned(),
            native_format_id: Some(13),
            bytes: vec![0, 0],
        };
        let formats = build_formats(&[payload.clone(), payload], None).expect("格式构造应成功");
        assert_eq!(formats.len(), 1);
    }
}
