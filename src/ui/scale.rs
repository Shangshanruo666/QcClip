use gpui::{App, Pixels, Rems, Window, px, rems};

/// 将默认 16px 基准下的界面尺寸转换为 rem，随主题字号统一缩放。
pub(crate) fn ui_rem(value: f32) -> Rems {
    rems(value / 16.0)
}

/// 用于布局测量、虚拟列表和动画，保持计算值与 rem 样式一致。
pub(crate) fn window_scale(window: &Window) -> f32 {
    f32::from(window.rem_size()) / 16.0
}

pub(crate) fn theme_scale(cx: &App) -> f32 {
    f32::from(gpui_component::Theme::global(cx).font_size) / 16.0
}

/// 独立弹出窗口保留自己的字号基准，并应用用户设置的界面缩放。
pub(crate) fn standalone_rem_size(base: f32, cx: &App) -> Pixels {
    px(base * crate::config_runtime::appearance_config(cx).ui_scale())
}

/// 悬停预览窗口的字号基准，在界面缩放之上再叠加预览缩放。
pub(crate) fn preview_rem_size(base: f32, cx: &App) -> Pixels {
    px(base * crate::config_runtime::appearance_config(cx).preview_scale())
}
