//! 图片便签的缩放与平移计算。
//!
//! 缩放倍数相对“完整显示在窗口内”的尺寸；1 倍时图片居中适配窗口，
//! 放大后围绕鼠标位置缩放，并允许拖动平移查看局部。

use gpui::{Bounds, Pixels, Point, Size, point, px, size};

pub(super) const MIN_SCALE: f32 = 1.0;
pub(super) const MAX_SCALE: f32 = 32.0;
const WHEEL_STEP: f32 = 1.15;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ImageView {
    /// 相对适配尺寸的放大倍数。
    pub(super) scale: f32,
    /// 图片中心相对视口中心的偏移。
    pub(super) offset: Point<Pixels>,
}

impl Default for ImageView {
    fn default() -> Self {
        Self {
            scale: MIN_SCALE,
            offset: Point::default(),
        }
    }
}

impl ImageView {
    pub(super) fn is_zoomed(&self) -> bool {
        self.scale > MIN_SCALE + f32::EPSILON
    }

    /// 以视口内的 `anchor` 为中心滚轮缩放，返回是否发生变化。
    pub(super) fn zoom_at(
        &mut self,
        wheel_delta: f32,
        anchor: Point<Pixels>,
        viewport: Size<Pixels>,
        image: Size<f32>,
    ) -> bool {
        if wheel_delta == 0.0 {
            return false;
        }
        let factor = if wheel_delta > 0.0 {
            WHEEL_STEP
        } else {
            1.0 / WHEEL_STEP
        };
        let scale = (self.scale * factor).clamp(MIN_SCALE, MAX_SCALE);
        if (scale - self.scale).abs() < f32::EPSILON {
            return false;
        }
        // 保持鼠标下的图片点不动：偏移按缩放比例围绕锚点重新计算。
        let center = point(viewport.width / 2.0, viewport.height / 2.0);
        let from_center = anchor - center;
        let ratio = scale / self.scale;
        self.offset = point(
            from_center.x - (from_center.x - self.offset.x) * ratio,
            from_center.y - (from_center.y - self.offset.y) * ratio,
        );
        self.scale = scale;
        self.clamp(viewport, image);
        true
    }

    pub(super) fn pan(&mut self, delta: Point<Pixels>, viewport: Size<Pixels>, image: Size<f32>) {
        self.offset = self.offset + delta;
        self.clamp(viewport, image);
    }

    /// 图片在视口内的绘制区域。
    pub(super) fn image_bounds(&self, viewport: Size<Pixels>, image: Size<f32>) -> Bounds<Pixels> {
        let displayed = displayed_size(viewport, image, self.scale);
        let origin = point(
            (viewport.width - displayed.width) / 2.0 + self.offset.x,
            (viewport.height - displayed.height) / 2.0 + self.offset.y,
        );
        Bounds::new(origin, displayed)
    }

    /// 图片比视口大时不允许拖出空白，比视口小时保持居中。
    fn clamp(&mut self, viewport: Size<Pixels>, image: Size<f32>) {
        let displayed = displayed_size(viewport, image, self.scale);
        let limit = |displayed: Pixels, viewport: Pixels, offset: Pixels| {
            let overflow = ((displayed - viewport) / 2.0).max(px(0.0));
            offset.clamp(-overflow, overflow)
        };
        self.offset = point(
            limit(displayed.width, viewport.width, self.offset.x),
            limit(displayed.height, viewport.height, self.offset.y),
        );
        if !self.is_zoomed() {
            self.scale = MIN_SCALE;
            self.offset = Point::default();
        }
    }
}

/// 按倍数缩放后的显示尺寸；1 倍时完整适配视口并保持宽高比。
fn displayed_size(viewport: Size<Pixels>, image: Size<f32>, scale: f32) -> Size<Pixels> {
    if image.width <= 0.0 || image.height <= 0.0 {
        return viewport;
    }
    let fit = (f32::from(viewport.width) / image.width)
        .min(f32::from(viewport.height) / image.height)
        .max(0.0);
    size(
        px(image.width * fit * scale),
        px(image.height * fit * scale),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewport() -> Size<Pixels> {
        size(px(400.0), px(300.0))
    }

    fn image() -> Size<f32> {
        size(800.0, 600.0)
    }

    #[test]
    fn default_view_fits_and_centers_the_image() {
        let view = ImageView::default();
        let bounds = view.image_bounds(viewport(), image());
        assert_eq!(bounds.size, viewport());
        assert_eq!(bounds.origin, Point::default());
        let tall = view.image_bounds(viewport(), size(100.0, 300.0));
        assert_eq!(tall.size, size(px(100.0), px(300.0)));
        assert_eq!(tall.origin.x, px(150.0));
    }

    #[test]
    fn zooming_keeps_the_point_under_the_cursor_fixed() {
        let mut view = ImageView::default();
        let anchor = point(px(100.0), px(80.0));
        let before = view.image_bounds(viewport(), image());
        let relative_x = (anchor.x - before.origin.x) / before.size.width;
        let relative_y = (anchor.y - before.origin.y) / before.size.height;
        assert!(view.zoom_at(1.0, anchor, viewport(), image()));
        let after = view.image_bounds(viewport(), image());
        let point_x = after.origin.x + after.size.width * relative_x;
        let point_y = after.origin.y + after.size.height * relative_y;
        assert!((f32::from(point_x) - 100.0).abs() < 0.01);
        assert!((f32::from(point_y) - 80.0).abs() < 0.01);
    }

    #[test]
    fn scale_is_clamped_and_zooming_out_recenters() {
        let mut view = ImageView::default();
        assert!(!view.zoom_at(-1.0, point(px(10.0), px(10.0)), viewport(), image()));
        for _ in 0..100 {
            view.zoom_at(1.0, point(px(10.0), px(10.0)), viewport(), image());
        }
        assert_eq!(view.scale, MAX_SCALE);
        for _ in 0..100 {
            view.zoom_at(-1.0, point(px(390.0), px(290.0)), viewport(), image());
        }
        assert_eq!(view, ImageView::default());
    }

    #[test]
    fn panning_cannot_reveal_empty_space() {
        let mut view = ImageView {
            scale: 2.0,
            offset: Point::default(),
        };
        view.pan(point(px(1000.0), px(-1000.0)), viewport(), image());
        let bounds = view.image_bounds(viewport(), image());
        assert_eq!(bounds.origin.x, px(0.0));
        assert_eq!(bounds.origin.y + bounds.size.height, viewport().height);
        let mut fitted = ImageView::default();
        fitted.pan(point(px(50.0), px(50.0)), viewport(), image());
        assert_eq!(fitted, ImageView::default());
    }
}
