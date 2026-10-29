use std::time::Instant;

use gpui::{
    Along as _, Axis, Bounds, IsZero, Pixels, Point, ScrollHandle, ScrollWheelEvent, TouchPhase,
    Window, px,
};
use gpui_component::VirtualListScrollHandle;

const SMOOTHING_SECONDS: f32 = 0.11;
const SETTLE_DISTANCE: f32 = 0.5;
const MAX_FRAME_SECONDS: f32 = 0.1;
/// 单次滚轮最多滚动可视区一页，并保留部分重叠，与平台层及 Chromium 的翻页步长一致。
const PAGE_MIN_FRACTION: f32 = 0.875;
const PAGE_MAX_OVERLAP: f32 = 40.0;

pub(crate) trait SmoothScrollTarget {
    fn offset(&self) -> Point<Pixels>;
    fn max_offset(&self) -> Point<Pixels>;
    fn bounds(&self) -> Bounds<Pixels>;
    fn set_offset(&self, offset: Point<Pixels>);
}

impl SmoothScrollTarget for ScrollHandle {
    fn offset(&self) -> Point<Pixels> {
        self.offset()
    }

    fn max_offset(&self) -> Point<Pixels> {
        self.max_offset()
    }

    fn bounds(&self) -> Bounds<Pixels> {
        self.bounds()
    }

    fn set_offset(&self, offset: Point<Pixels>) {
        self.set_offset(offset);
    }
}

impl SmoothScrollTarget for VirtualListScrollHandle {
    fn offset(&self) -> Point<Pixels> {
        self.as_ref().offset()
    }

    fn max_offset(&self) -> Point<Pixels> {
        self.as_ref().max_offset()
    }

    fn bounds(&self) -> Bounds<Pixels> {
        self.as_ref().bounds()
    }

    fn set_offset(&self, offset: Point<Pixels>) {
        self.as_ref().set_offset(offset);
    }
}

#[derive(Clone, Copy)]
struct ScrollAnimation {
    target: Pixels,
    last_offset: Pixels,
    last_frame_at: Instant,
}

pub(crate) struct SmoothScroll {
    enabled: bool,
    animation: Option<ScrollAnimation>,
    frame_scheduled: bool,
    axis: Axis,
    touch_gesture_active: bool,
}

impl SmoothScroll {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            animation: None,
            frame_scheduled: false,
            axis: Axis::Vertical,
            touch_gesture_active: false,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.animation = None;
        self.frame_scheduled = false;
    }

    pub(crate) fn set_axis(&mut self, axis: Axis) {
        if self.axis != axis {
            self.reset();
            self.axis = axis;
        }
    }

    pub(crate) fn handle_wheel<T: SmoothScrollTarget>(
        &mut self,
        scroll_handle: &T,
        event: &ScrollWheelEvent,
        window: &Window,
        animations_enabled: bool,
    ) -> bool {
        self.track_touch_phase(event.touch_phase);
        let mut delta = wheel_delta(
            event.delta.pixel_delta(window.line_height()),
            self.axis,
            event.modifiers.shift,
        );
        if delta.is_zero() {
            return false;
        }
        let touch_gesture = event.delta.precise() && self.touch_gesture_active;
        if !touch_gesture {
            let page = page_step(scroll_handle.bounds().size.along(self.axis));
            delta = delta.clamp(-page, page);
        }

        let current_offset = scroll_handle.offset();
        let current = current_offset.along(self.axis);
        let max_offset = scroll_handle.max_offset().along(self.axis);
        let should_smooth = self.enabled && animations_enabled && !touch_gesture;
        let target = if should_smooth {
            let target = self
                .animation
                .filter(|animation| animation.last_offset == current)
                .map(|animation| animation.target)
                .unwrap_or(current);
            (target + delta).clamp(-max_offset, Pixels::ZERO)
        } else {
            (current + delta).clamp(-max_offset, Pixels::ZERO)
        };

        if should_smooth {
            let last_frame_at = self
                .animation
                .filter(|animation| animation.last_offset == current)
                .map(|animation| animation.last_frame_at)
                .unwrap_or_else(Instant::now);
            self.animation = Some(ScrollAnimation {
                target,
                last_offset: current,
                last_frame_at,
            });
            self.schedule_next_frame(window);
        } else {
            scroll_handle.set_offset(current_offset.apply_along(self.axis, |_| target));
            self.animation = None;
        }

        true
    }

    // 触控板手势以 Started 开始、Ended 结束；Windows 滚轮也给出像素增量，但不处于手势序列中。
    fn track_touch_phase(&mut self, touch_phase: TouchPhase) {
        match touch_phase {
            TouchPhase::Started => self.touch_gesture_active = true,
            TouchPhase::Ended | TouchPhase::Cancelled => self.touch_gesture_active = false,
            TouchPhase::Moved => {}
        }
    }

    pub(crate) fn advance<T: SmoothScrollTarget>(&mut self, scroll_handle: &T, window: &Window) {
        self.frame_scheduled = false;
        let Some(mut animation) = self.animation.take() else {
            return;
        };

        let current_offset = scroll_handle.offset();
        let current = current_offset.along(self.axis);
        if current != animation.last_offset {
            return;
        }

        let max_offset = scroll_handle.max_offset().along(self.axis);
        animation.target = animation.target.clamp(-max_offset, Pixels::ZERO);
        let now = Instant::now();
        let elapsed = now
            .saturating_duration_since(animation.last_frame_at)
            .as_secs_f32()
            .min(MAX_FRAME_SECONDS);
        animation.last_frame_at = now;
        let distance = animation.target - current;
        if distance.abs() <= px(SETTLE_DISTANCE) {
            scroll_handle.set_offset(current_offset.apply_along(self.axis, |_| animation.target));
            return;
        }

        let progress = 1.0 - (-elapsed / SMOOTHING_SECONDS).exp();
        let offset = current_offset.apply_along(self.axis, |value| value + distance * progress);
        animation.last_offset = offset.along(self.axis);
        scroll_handle.set_offset(offset);
        self.animation = Some(animation);
        self.schedule_next_frame(window);
    }

    fn schedule_next_frame(&mut self, window: &Window) {
        if self.frame_scheduled {
            return;
        }

        self.frame_scheduled = true;
        // 滚轮回调不一定处在视图渲染栈中，不能调用依赖 current_view 的动画帧 API。
        window.on_next_frame(|window, _| window.refresh());
    }
}

fn page_step(extent: Pixels) -> Pixels {
    (extent * PAGE_MIN_FRACTION)
        .max(extent - px(PAGE_MAX_OVERLAP))
        .max(Pixels::ZERO)
}

/// 平台层在按住 Shift 时会把纵向滚轮转换为横向增量，纵向列表需要换回纵轴。
pub(crate) fn vertical_wheel_delta(delta: Point<Pixels>, shift: bool) -> Pixels {
    if shift && delta.y.is_zero() {
        delta.x
    } else {
        delta.y
    }
}

fn wheel_delta(delta: Point<Pixels>, axis: Axis, shift: bool) -> Pixels {
    match axis {
        Axis::Vertical => vertical_wheel_delta(delta, shift),
        // 横向触控板使用横轴，普通滚轮和以纵轴为主的手势映射到横轴。
        Axis::Horizontal if delta.x.abs() > delta.y.abs() => delta.x,
        Axis::Horizontal => delta.y,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizontal_scrolling_handles_mouse_wheel_and_trackpad_without_double_counting() {
        for (x, y, expected) in [(0.0, -40.0, -40.0), (-30.0, -2.0, -30.0), (2.0, 30.0, 30.0)] {
            let delta = Point { x: px(x), y: px(y) };
            assert_eq!(wheel_delta(delta, Axis::Horizontal, false), px(expected));
            assert_eq!(wheel_delta(delta, Axis::Vertical, false), px(y));
        }
    }

    #[test]
    fn vertical_scrolling_accepts_shift_converted_wheel() {
        let shifted = Point {
            x: px(-40.0),
            y: px(0.0),
        };
        assert_eq!(wheel_delta(shifted, Axis::Vertical, true), px(-40.0));
        assert_eq!(wheel_delta(shifted, Axis::Horizontal, true), px(-40.0));
        assert_eq!(wheel_delta(shifted, Axis::Vertical, false), px(0.0));
    }

    #[test]
    fn only_touch_gestures_skip_smoothing() {
        let mut scroll = SmoothScroll::default();
        for (touch_phase, active) in [
            (TouchPhase::Moved, false),
            (TouchPhase::Started, true),
            (TouchPhase::Moved, true),
            (TouchPhase::Ended, false),
        ] {
            scroll.track_touch_phase(touch_phase);
            assert_eq!(scroll.touch_gesture_active, active);
        }
    }

    #[test]
    fn page_step_keeps_overlap() {
        assert_eq!(page_step(px(200.0)), px(175.0));
        assert_eq!(page_step(px(800.0)), px(760.0));
        assert_eq!(page_step(px(0.0)), px(0.0));
    }

    #[test]
    fn switching_scroll_axis_cancels_the_previous_animation() {
        let mut scroll = SmoothScroll::default();
        scroll.animation = Some(ScrollAnimation {
            target: px(-100.0),
            last_offset: px(-20.0),
            last_frame_at: Instant::now(),
        });
        scroll.set_axis(Axis::Horizontal);
        assert!(scroll.animation.is_none());
    }
}

impl Default for SmoothScroll {
    fn default() -> Self {
        Self::new(true)
    }
}
