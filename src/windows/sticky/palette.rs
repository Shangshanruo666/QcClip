use gpui::{App, ColorExt as _, Hsla, hsla, rgb};
use gpui_component::ActiveTheme as _;

/// 便签窗口配色。
///
/// 未设置颜色时直接使用当前主题；设置后以用户颜色的色相与饱和度生成一组低饱和度纸面色，
/// 浅色和深色模式分别推导，保证文字始终可读。开启窗口材质时纸面与标题栏会按主题表面的
/// 透明度变淡，与主题窗口一样透出材质。
#[derive(Clone, Copy)]
pub(super) struct Paper {
    pub(super) background: Hsla,
    pub(super) header: Hsla,
    pub(super) border: Hsla,
    pub(super) foreground: Hsla,
    pub(super) muted: Hsla,
    pub(super) accent: Hsla,
    pub(super) accent_foreground: Hsla,
    /// 标题栏颜色按钮里显示的色块；跟随主题时为空。
    pub(super) swatch: Option<Hsla>,
    /// 开启窗口材质时主遮罩使用的色相；跟随主题时为空，沿用主题背景。
    pub(super) tint: Option<Hsla>,
}

impl Paper {
    pub(super) fn resolve(color: Option<u32>, cx: &App) -> Self {
        let theme = cx.theme().colors;
        let Some(color) = color else {
            return Self {
                background: theme.background,
                header: theme.title_bar,
                border: theme.border,
                foreground: theme.foreground,
                muted: theme.muted_foreground,
                accent: theme.primary,
                accent_foreground: theme.primary_foreground,
                swatch: None,
                tint: None,
            };
        };
        let mut paper = Self::from_color(color, cx.theme().mode.is_dark());
        paper.tint = Some(paper.background);
        // 主题背景在开启材质时已被调低透明度；纸面沿用同样的比例，否则会盖住材质。
        let base = crate::window_material::excluded_window_theme(cx);
        paper.background = paper
            .background
            .opacity(surface_ratio(theme.background, base.background));
        paper.header = paper
            .header
            .opacity(surface_ratio(theme.title_bar, base.title_bar));
        paper
    }

    fn from_color(color: u32, dark: bool) -> Self {
        let source = gpui::rgb_to_hsla(rgb(color));
        let hue = f32::from(source.color.hue.into_positive_degrees()) / 360.0;
        // 灰色等低饱和度颜色保持中性，不额外染色。
        let saturation = source.color.saturation;
        let tone = |s: f32, l: f32| hsla(hue, (saturation * s).min(s), l, 1.0);
        let accent_saturation = saturation.clamp(0.0, 0.85);
        if dark {
            Self {
                background: tone(0.32, 0.15),
                header: tone(0.32, 0.19),
                border: tone(0.28, 0.27),
                foreground: tone(0.45, 0.90),
                muted: tone(0.22, 0.66),
                accent: hsla(hue, accent_saturation, 0.64, 1.0),
                accent_foreground: hsla(hue, 0.3, 0.1, 1.0),
                swatch: Some(source),
                tint: None,
            }
        } else {
            Self {
                background: tone(0.85, 0.93),
                header: tone(0.80, 0.88),
                border: tone(0.55, 0.78),
                foreground: tone(0.45, 0.15),
                muted: tone(0.20, 0.40),
                accent: hsla(hue, accent_saturation, 0.42, 1.0),
                accent_foreground: hsla(0.0, 0.0, 1.0, 1.0),
                swatch: Some(source),
                tint: None,
            }
        }
    }

    pub(super) fn token(&self) -> Hsla {
        self.foreground.opacity(0.07)
    }

    pub(super) fn hover(&self) -> Hsla {
        self.foreground.opacity(0.1)
    }
}

/// 材质处理后的主题表面相对原始主题色保留的透明度比例。
fn surface_ratio(current: Hsla, base: Hsla) -> f32 {
    if base.alpha <= f32::EPSILON {
        return 1.0;
    }
    (current.alpha / base.alpha).clamp(0.0, 1.0)
}

pub(super) fn hsla_to_rgb(color: Hsla) -> u32 {
    let color = gpui::hsla_to_rgba(color);
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u32;
    (channel(color.color.red) << 16) | (channel(color.color.green) << 8) | channel(color.color.blue)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lightness(color: Hsla) -> f32 {
        color.color.lightness
    }

    #[test]
    fn custom_colors_keep_text_readable_in_both_modes() {
        for color in [0xFF0000, 0x00FF00, 0x0000FF, 0xFFEFA0, 0x111111, 0xFFFFFF] {
            let light = Paper::from_color(color, false);
            assert!(lightness(light.background) - lightness(light.foreground) > 0.6);
            let dark = Paper::from_color(color, true);
            assert!(lightness(dark.foreground) - lightness(dark.background) > 0.6);
        }
    }

    #[test]
    fn neutral_colors_stay_neutral() {
        let paper = Paper::from_color(0x808080, false);
        assert!(paper.background.color.saturation < 0.01);
    }

    #[test]
    fn surface_ratio_follows_theme_transparency() {
        let base = hsla(0.0, 0.0, 1.0, 1.0);
        assert_eq!(surface_ratio(base, base), 1.0);
        assert!((surface_ratio(base.opacity(0.06), base) - 0.06).abs() < 1e-4);
        assert_eq!(surface_ratio(base, hsla(0.0, 0.0, 1.0, 0.0)), 1.0);
    }

    #[test]
    fn rgb_round_trips_through_hsla() {
        for color in [0x000000, 0xFFFFFF, 0x3B82F6, 0xDB2D6A] {
            assert_eq!(hsla_to_rgb(gpui::rgb_to_hsla(rgb(color))), color);
        }
    }
}
