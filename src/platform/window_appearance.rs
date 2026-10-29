use crate::config::WindowMaterial;

#[cfg(any(target_os = "windows", test))]
const CORNER_RADIUS: f32 = 8.0;

/// 原生圆角从 Windows 11 21H2 开始支持，与窗口材质的版本分档独立。
#[cfg(target_os = "windows")]
pub(super) fn supports_native_rounding() -> bool {
    static CACHED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| native_rounding_for_build(super::os_version::build_number()))
}

#[cfg(any(target_os = "windows", test))]
fn native_rounding_for_build(build: Option<u32>) -> bool {
    matches!(build, Some(22000..))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FrameStyle {
    pub(crate) corner_radius: f32,
}

#[cfg(any(target_os = "windows", test))]
fn frame_policy(
    native_rounding: bool,
    material: WindowMaterial,
    decorations_visible: bool,
    maximized: bool,
    fullscreen: bool,
) -> Option<FrameStyle> {
    if native_rounding || !decorations_visible || fullscreen {
        return None;
    }
    let square = maximized || matches!(material, WindowMaterial::Acrylic | WindowMaterial::Blur);
    Some(FrameStyle {
        corner_radius: if square { 0.0 } else { CORNER_RADIUS },
    })
}

pub(crate) fn apply(window: &gpui::Window, material: WindowMaterial) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    if !supports_native_rounding() {
        return native::apply(window, material);
    }
    let _ = (window, material);
    Ok(())
}

pub(crate) fn frame_style(window: &gpui::Window) -> Option<FrameStyle> {
    #[cfg(target_os = "windows")]
    if !supports_native_rounding() {
        return native::frame_style(window);
    }
    let _ = window;
    None
}

#[cfg(target_os = "windows")]
pub(super) fn set_decorations_visible(
    hwnd: windows::Win32::Foundation::HWND,
    visible: bool,
) -> Result<(), String> {
    native::set_decorations_visible(hwnd, visible)
}

/// 旧系统没有 DWM 原生圆角。GDI 窗口区域只能按整像素裁剪，边缘会有锯齿，
/// 因此改由 GPUI 渲染器在每帧末尾按圆角矩形做抗锯齿透明遮罩。
#[cfg(target_os = "windows")]
mod native {
    use super::{FrameStyle, WindowMaterial, frame_policy};
    use windows::Win32::{
        Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM},
        Graphics::Gdi::InvalidateRect,
        UI::{
            HiDpi::GetDpiForWindow,
            Shell::{DefSubclassProc, GetWindowSubclass, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::{
                IsIconic, IsZoomed, RemovePropW, SetPropW, WM_DPICHANGED, WM_NCDESTROY, WM_SIZE,
                WM_WINDOWPOSCHANGED,
            },
        },
    };

    const SUBCLASS_ID: usize = 1;
    /// 与 vendor/gpui-windows 渲染器约定的窗口属性，值为物理像素圆角半径。
    const CORNER_RADIUS_PROP: windows::core::PCWSTR = windows::core::w!("GpuiWindowCornerRadius");

    fn physical_radius(dpi: u32) -> usize {
        (super::CORNER_RADIUS * dpi.max(96) as f32 / 96.0).round() as usize
    }

    #[derive(Clone, Copy)]
    struct AppearanceState {
        material: WindowMaterial,
        decorations_visible: bool,
        fullscreen: bool,
        radius: usize,
        rounding_failed: bool,
    }

    fn state(hwnd: HWND) -> Option<*mut AppearanceState> {
        let mut data = 0;
        // SAFETY: 只查询本模块在当前窗口线程安装的子类状态。
        unsafe {
            GetWindowSubclass(hwnd, Some(window_proc), SUBCLASS_ID, Some(&mut data))
                .as_bool()
                .then_some(data as *mut AppearanceState)
        }
    }

    fn install(hwnd: HWND, material: WindowMaterial, fullscreen: bool) -> Result<(), String> {
        let data = Box::into_raw(Box::new(AppearanceState {
            material,
            decorations_visible: true,
            fullscreen,
            radius: 0,
            rounding_failed: false,
        }));
        // SAFETY: 状态地址保持稳定，由 WM_NCDESTROY 回收；子类仅处理当前线程的窗口。
        if !unsafe { SetWindowSubclass(hwnd, Some(window_proc), SUBCLASS_ID, data as usize) }
            .as_bool()
        {
            // SAFETY: 安装失败时系统未取得状态所有权。
            unsafe { drop(Box::from_raw(data)) };
            return Err("安装窗口外观处理失败".to_owned());
        }
        Ok(())
    }

    pub(super) fn apply(window: &gpui::Window, material: WindowMaterial) -> Result<(), String> {
        let hwnd = super::super::window::window_hwnd(window)?;
        apply_to_hwnd(hwnd, material, window.is_fullscreen())
    }

    fn apply_to_hwnd(hwnd: HWND, material: WindowMaterial, fullscreen: bool) -> Result<(), String> {
        if let Some(data) = state(hwnd) {
            // SAFETY: 状态由当前窗口线程独占，修改期间不调用可能重入的系统接口。
            unsafe {
                if (*data).material != material {
                    (*data).rounding_failed = false;
                }
                (*data).material = material;
                (*data).fullscreen = fullscreen;
            }
        } else {
            install(hwnd, material, fullscreen)?;
        }
        refresh(hwnd)
    }

    pub(super) fn set_decorations_visible(hwnd: HWND, visible: bool) -> Result<(), String> {
        if let Some(data) = state(hwnd) {
            // SAFETY: 仅在窗口线程修改仍有效的状态，随后才调用系统接口。
            unsafe { (*data).decorations_visible = visible };
            refresh(hwnd)?;
        }
        Ok(())
    }

    fn current_style(hwnd: HWND) -> Option<FrameStyle> {
        let data = state(hwnd)?;
        // SAFETY: 复制状态后再查询系统，避免跨窗口消息持有 Rust 引用。
        let snapshot = unsafe { *data };
        // SAFETY: hwnd 来自当前窗口，仅读取最大化状态。
        let maximized = unsafe { IsZoomed(hwnd).as_bool() };
        let mut style = frame_policy(
            false,
            snapshot.material,
            snapshot.decorations_visible,
            maximized,
            snapshot.fullscreen,
        )?;
        if snapshot.rounding_failed {
            style.corner_radius = 0.0;
        }
        Some(style)
    }

    pub(super) fn frame_style(window: &gpui::Window) -> Option<FrameStyle> {
        let hwnd = super::super::window::window_hwnd(window).ok()?;
        let fullscreen = window.is_fullscreen();
        let Some(data) = state(hwnd) else {
            // 子类安装失败时仍保留直角边框，保证窗口与桌面有清晰边界。
            return (!fullscreen).then_some(FrameStyle { corner_radius: 0.0 });
        };
        // SAFETY: 在当前窗口线程读取状态，不持有跨系统调用的引用。
        if unsafe { (*data).fullscreen != fullscreen } {
            unsafe { (*data).fullscreen = fullscreen };
            if let Err(error) = refresh(hwnd) {
                tracing::warn!("同步全屏窗口外观失败：{error}");
            }
        }
        current_style(hwnd)
    }

    fn refresh(hwnd: HWND) -> Result<(), String> {
        let Some(data) = state(hwnd) else {
            return Ok(());
        };
        // SAFETY: 仅读取当前窗口是否处于最小化状态。
        if unsafe { IsIconic(hwnd).as_bool() } {
            return Ok(());
        }
        let target = if current_style(hwnd).is_some_and(|style| style.corner_radius > 0.0) {
            // SAFETY: hwnd 来自当前窗口，仅读取其物理 DPI。
            physical_radius(unsafe { GetDpiForWindow(hwnd) })
        } else {
            0
        };
        // SAFETY: 当前窗口线程独占状态；设置窗口属性不会同步派发窗口消息。
        if unsafe { (*data).radius } == target {
            return Ok(());
        }
        let result = set_radius(hwnd, target);
        // SAFETY: 同上，期间没有可能重入的窗口消息。
        unsafe {
            if result.is_ok() {
                (*data).radius = target;
            } else {
                // 设置失败时边框回退直角，并尽量清除旧半径，避免遮罩与边框不一致。
                (*data).rounding_failed = true;
                if set_radius(hwnd, 0).is_ok() {
                    (*data).radius = 0;
                }
            }
            // 遮罩在渲染阶段生效，请求重绘让新半径立即体现。
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
        result
    }

    fn set_radius(hwnd: HWND, radius: usize) -> Result<(), String> {
        if radius == 0 {
            // SAFETY: 移除本模块设置的整数属性，属性不存在时无副作用。
            let _ = unsafe { RemovePropW(hwnd, CORNER_RADIUS_PROP) };
            return Ok(());
        }
        // SAFETY: 属性只保存整数值而非指针，窗口销毁时由 WM_NCDESTROY 移除。
        unsafe {
            SetPropW(
                hwnd,
                CORNER_RADIUS_PROP,
                Some(HANDLE(radius as *mut std::ffi::c_void)),
            )
        }
        .map_err(|error| format!("更新窗口圆角失败：{error}"))
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        subclass_id: usize,
        data: usize,
    ) -> LRESULT {
        if message == WM_NCDESTROY {
            // SAFETY: 状态由 install 分配，仅在移除子类后回收一次。
            unsafe {
                let _ = RemovePropW(hwnd, CORNER_RADIUS_PROP);
                let _ = RemoveWindowSubclass(hwnd, Some(window_proc), subclass_id);
                drop(Box::from_raw(data as *mut AppearanceState));
                return DefSubclassProc(hwnd, message, wparam, lparam);
            }
        }
        // SAFETY: 原样转发消息，待 GPUI 和已有窗口钩子更新尺寸及 DPI 后再同步圆角。
        let result = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
        if matches!(message, WM_SIZE | WM_WINDOWPOSCHANGED | WM_DPICHANGED)
            && let Err(error) = refresh(hwnd)
        {
            tracing::warn!("更新窗口外观失败：{error}");
        }
        result
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, GetPropW, SW_MAXIMIZE, SW_RESTORE, ShowWindow, WS_POPUP,
        };

        struct TestWindow(HWND);

        impl TestWindow {
            fn new() -> Self {
                // SAFETY: 创建当前测试线程专用的隐藏窗口，由 Drop 在同一线程销毁。
                Self(unsafe {
                    CreateWindowExW(
                        Default::default(),
                        windows::core::w!("STATIC"),
                        windows::core::w!("窗口外观测试"),
                        WS_POPUP,
                        0,
                        0,
                        320,
                        200,
                        None,
                        None,
                        None,
                        None,
                    )
                    .expect("创建窗口外观测试窗口失败")
                })
            }

            fn radius(&self) -> usize {
                // SAFETY: 只读取本测试窗口的整数属性。
                unsafe { GetPropW(self.0, CORNER_RADIUS_PROP) }.0 as usize
            }

            fn is_rounded(&self) -> bool {
                // SAFETY: hwnd 是当前测试线程创建的有效窗口。
                self.radius() == physical_radius(unsafe { GetDpiForWindow(self.0) })
            }
        }

        impl Drop for TestWindow {
            fn drop(&mut self) {
                // SAFETY: 窗口由当前线程创建，销毁消息同时回收子类状态。
                let _ = unsafe { DestroyWindow(self.0) };
            }
        }

        #[test]
        fn native_material_switches_remove_and_restore_the_corner_radius() {
            let window = TestWindow::new();
            for material in [
                WindowMaterial::Disabled,
                WindowMaterial::Transparent,
                WindowMaterial::CustomImage,
            ] {
                apply_to_hwnd(window.0, material, false).unwrap();
                assert!(window.is_rounded());
                for system_material in [WindowMaterial::Acrylic, WindowMaterial::Blur] {
                    apply_to_hwnd(window.0, system_material, false).unwrap();
                    assert_eq!(window.radius(), 0);
                    assert_eq!(current_style(window.0).unwrap().corner_radius, 0.0);
                    apply_to_hwnd(window.0, material, false).unwrap();
                    assert!(window.is_rounded());
                }
            }
        }

        #[test]
        fn maximize_and_visibility_changes_keep_the_radius_in_sync() {
            let window = TestWindow::new();
            apply_to_hwnd(window.0, WindowMaterial::Disabled, false).unwrap();
            assert!(window.is_rounded());
            // SAFETY: 只改变本测试窗口的显示状态，验证同步位置消息会更新圆角。
            unsafe {
                let _ = ShowWindow(window.0, SW_MAXIMIZE);
            }
            assert_eq!(window.radius(), 0);
            // SAFETY: 同上。
            unsafe {
                let _ = ShowWindow(window.0, SW_RESTORE);
            }
            assert!(window.is_rounded());
            set_decorations_visible(window.0, false).unwrap();
            assert_eq!(window.radius(), 0);
            assert!(current_style(window.0).is_none());
            set_decorations_visible(window.0, true).unwrap();
            assert!(window.is_rounded());
            apply_to_hwnd(window.0, WindowMaterial::Blur, false).unwrap();
            set_decorations_visible(window.0, false).unwrap();
            set_decorations_visible(window.0, true).unwrap();
            assert_eq!(window.radius(), 0);
            assert_eq!(current_style(window.0).unwrap().corner_radius, 0.0);
        }

        #[test]
        fn fullscreen_clears_the_radius_until_restored() {
            let window = TestWindow::new();
            apply_to_hwnd(window.0, WindowMaterial::Disabled, false).unwrap();
            assert!(window.is_rounded());
            apply_to_hwnd(window.0, WindowMaterial::Disabled, true).unwrap();
            assert_eq!(window.radius(), 0);
            assert!(current_style(window.0).is_none());
            apply_to_hwnd(window.0, WindowMaterial::Disabled, false).unwrap();
            assert!(window.is_rounded());
        }

        #[test]
        fn corner_radius_scales_with_dpi() {
            assert_eq!(physical_radius(96), 8);
            assert_eq!(physical_radius(144), 12);
            assert_eq!(physical_radius(192), 16);
            assert_eq!(physical_radius(0), 8);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_rounding_starts_at_windows11_21h2() {
        for build in [None, Some(7601), Some(19045), Some(21999)] {
            assert!(!native_rounding_for_build(build));
        }
        for build in [22000, 22621, 26100] {
            assert!(native_rounding_for_build(Some(build)));
        }
    }

    #[test]
    fn windows11_keeps_native_decorations_for_every_material() {
        for material in WindowMaterial::ALL {
            assert!(frame_policy(true, material, true, false, false).is_none());
        }
    }

    #[test]
    fn legacy_system_materials_are_square_and_ui_backgrounds_are_rounded() {
        for material in WindowMaterial::ALL {
            let style = frame_policy(false, material, true, false, false).unwrap();
            let expected = match material {
                WindowMaterial::Acrylic | WindowMaterial::Blur => 0.0,
                _ => CORNER_RADIUS,
            };
            assert_eq!(style.corner_radius, expected);
        }
    }

    #[test]
    fn maximized_windows_are_square_and_hidden_or_fullscreen_windows_have_no_frame() {
        for material in WindowMaterial::ALL {
            assert_eq!(
                frame_policy(false, material, true, true, false)
                    .unwrap()
                    .corner_radius,
                0.0
            );
            assert!(frame_policy(false, material, false, false, false).is_none());
            assert!(frame_policy(false, material, true, false, true).is_none());
        }
    }
}
