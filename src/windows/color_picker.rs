use crate::ui::scale::ui_rem;

use std::{
    f32::consts::TAU,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use gpui::prelude::*;
use gpui::{
    App, Bounds, ClipboardItem, ColorSpace, Context, Entity, IntoElement, MouseButton, Pixels,
    Point, Render, RenderImage, Window, canvas, div, linear_color_stop, linear_gradient, px, rgb,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme as _, Sizable as _, button::ButtonVariants as _};
use image::{Frame, RgbaImage};

use super::dialog::{
    self, DIALOG_PADDING, DialogMode, DialogWindowPosition, dialog_body, dialog_button,
    dialog_footer, dialog_header, dialog_primary_button,
};
use crate::i18n;

const COLOR_PICKER_WIDTH: f32 = 332.0;
const COLOR_WHEEL_SIZE: f32 = 144.0;
const COLOR_WHEEL_HANDLE_SIZE: f32 = 12.0;
const BRIGHTNESS_TRACK_HEIGHT: f32 = 6.0;
const BRIGHTNESS_HANDLE_SIZE: f32 = 14.0;
const INPUT_HEIGHT: f32 = 32.0;

pub(crate) fn open_color_picker_window(
    source_window: Option<&Window>,
    initial_color: u32,
    on_done: impl FnOnce(u32, &mut App) + 'static,
    cx: &mut App,
) -> Result<gpui::WindowHandle<gpui_component::Root>, String> {
    open_picker(
        source_window,
        initial_color,
        None,
        Box::new(move |color: Option<u32>, cx: &mut App| {
            if let Some(color) = color {
                on_done(color, cx);
            }
        }),
        cx,
    )
}

/// 带“恢复默认”按钮的颜色选择；选择默认时回调收到 `None`。
pub(crate) fn open_color_picker_window_with_reset(
    source_window: Option<&Window>,
    initial_color: u32,
    reset_label: String,
    on_done: impl FnOnce(Option<u32>, &mut App) + 'static,
    cx: &mut App,
) -> Result<gpui::WindowHandle<gpui_component::Root>, String> {
    open_picker(
        source_window,
        initial_color,
        Some(reset_label),
        Box::new(on_done),
        cx,
    )
}

fn open_picker(
    source_window: Option<&Window>,
    initial_color: u32,
    reset_label: Option<String>,
    on_done: DoneHandler,
    cx: &mut App,
) -> Result<gpui::WindowHandle<gpui_component::Root>, String> {
    dialog::open_adaptive_window(
        source_window,
        cx,
        DialogWindowPosition::Center,
        DialogMode::Modal,
        move |window, cx| {
            cx.new(|cx| ColorPickerPanel::new(initial_color, reset_label, on_done, window, cx))
        },
    )
}

type DoneHandler = Box<dyn FnOnce(Option<u32>, &mut App)>;

pub(crate) struct ColorPickerPanel {
    selection: ColorSelection,
    on_done: Option<DoneHandler>,
    reset_label: Option<String>,
    hex_input: Entity<InputState>,
    red_input: Entity<InputState>,
    green_input: Entity<InputState>,
    blue_input: Entity<InputState>,
    wheel_image_cache: ColorWheelImageCache,
    wheel_bounds: Bounds<Pixels>,
    brightness_bounds: Bounds<Pixels>,
    _subscriptions: Vec<gpui::Subscription>,
}

impl ColorPickerPanel {
    fn new(
        initial_color: u32,
        reset_label: Option<String>,
        on_done: DoneHandler,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let selection = ColorSelection::from_rgb(initial_color, None);
        let translator = i18n::translator(cx);
        let hex_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(translator.translate("color_picker.hex_placeholder"))
                .default_value(format!("#{color:06X}", color = selection.rgb()))
        });
        let red_input =
            cx.new(|cx| InputState::new(window, cx).default_value(selection.red().to_string()));
        let green_input =
            cx.new(|cx| InputState::new(window, cx).default_value(selection.green().to_string()));
        let blue_input =
            cx.new(|cx| InputState::new(window, cx).default_value(selection.blue().to_string()));

        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe_in(
            &hex_input,
            window,
            |this, input, event: &InputEvent, window, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }
                let Some(color) = parse_hex_color(input.read(cx).value().as_ref()) else {
                    return;
                };
                this.apply_rgb(color, window, cx);
            },
        ));

        for input in [red_input.clone(), green_input.clone(), blue_input.clone()] {
            subscriptions.push(cx.subscribe_in(
                &input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.apply_rgb_inputs(window, cx);
                    }
                },
            ));
        }

        let hex_input_for_focus = hex_input.clone();
        window.defer(cx, move |window, cx| {
            hex_input_for_focus.update(cx, |input, cx| {
                input.focus(window, cx);
            });
        });

        Self {
            selection,
            on_done: Some(on_done),
            reset_label,
            hex_input,
            red_input,
            green_input,
            blue_input,
            wheel_image_cache: ColorWheelImageCache::default(),
            wheel_bounds: Bounds::default(),
            brightness_bounds: Bounds::default(),
            _subscriptions: subscriptions,
        }
    }

    fn apply_rgb_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let red = parse_rgb_input(self.red_input.read(cx).value().as_ref());
        let green = parse_rgb_input(self.green_input.read(cx).value().as_ref());
        let blue = parse_rgb_input(self.blue_input.read(cx).value().as_ref());
        let (Some(red), Some(green), Some(blue)) = (red, green, blue) else {
            return;
        };

        self.apply_rgb(
            ((red as u32) << 16) | ((green as u32) << 8) | blue as u32,
            window,
            cx,
        );
    }

    fn apply_rgb(&mut self, color: u32, window: &mut Window, cx: &mut Context<Self>) {
        let color = color & 0x00ff_ffff;
        if color == self.selection.rgb() {
            return;
        }

        self.selection = ColorSelection::from_rgb(color, Some(self.selection.hue));
        self.sync_inputs(window, cx);
        cx.notify();
    }

    fn sync_inputs(&self, window: &mut Window, cx: &mut Context<Self>) {
        set_input_value_if_changed(
            &self.hex_input,
            format!("#{color:06X}", color = self.selection.rgb()),
            window,
            cx,
        );
        set_input_value_if_changed(
            &self.red_input,
            self.selection.red().to_string(),
            window,
            cx,
        );
        set_input_value_if_changed(
            &self.green_input,
            self.selection.green().to_string(),
            window,
            cx,
        );
        set_input_value_if_changed(
            &self.blue_input,
            self.selection.blue().to_string(),
            window,
            cx,
        );
    }

    fn on_wheel_mouse_down(
        &mut self,
        event: &gpui::MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        self.update_from_wheel_position(event.position, window, cx);
    }

    fn on_wheel_mouse_move(
        &mut self,
        event: &gpui::MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.dragging() {
            return;
        }
        cx.stop_propagation();
        self.update_from_wheel_position(event.position, window, cx);
    }

    fn update_from_wheel_position(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((hue, saturation)) = color_wheel_position(self.wheel_bounds, position) else {
            return;
        };
        self.selection.hue = hue;
        self.selection.saturation = saturation;
        self.commit_selection(window, cx);
    }

    fn on_brightness_mouse_down(
        &mut self,
        event: &gpui::MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        self.update_from_brightness_position(event.position, window, cx);
    }

    fn on_brightness_mouse_move(
        &mut self,
        event: &gpui::MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.dragging() {
            return;
        }
        cx.stop_propagation();
        self.update_from_brightness_position(event.position, window, cx);
    }

    fn update_from_brightness_position(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let width = self.brightness_bounds.size.width.as_f32();
        if width <= 0.0 {
            return;
        }
        let left = self.brightness_bounds.left().as_f32();
        self.selection.value = ((position.x.as_f32() - left) / width).clamp(0.0, 1.0);
        self.commit_selection(window, cx);
    }

    fn commit_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_inputs(window, cx);
        cx.notify();
    }

    fn random_color(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or(0x92d6_8ca2_47ac_5f31);
        let mixed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.selection.hue = ((mixed & 0xffff) as f32) / 65535.0;
        self.selection.saturation = 0.52 + (((mixed >> 16) & 0xff) as f32 / 255.0) * 0.38;
        self.selection.value = 0.72 + (((mixed >> 24) & 0xff) as f32 / 255.0) * 0.24;
        self.commit_selection(window, cx);
    }

    fn copy_hex(&self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(format!(
            "#{color:06X}",
            color = self.selection.rgb()
        )));
    }

    fn done(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let color = self.selection.rgb();
        self.finish(Some(color), window, cx);
    }

    fn finish(&mut self, color: Option<u32>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(on_done) = self.on_done.take() {
            on_done(color, cx);
        }
        window.remove_window();
    }

    fn render_color_wheel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let entity = cx.entity().clone();
        let (handle_left, handle_top) = wheel_handle_position(self.wheel_bounds, self.selection);
        let wheel_image = self.wheel_image_cache.image(
            COLOR_WHEEL_SIZE,
            window.scale_factor().max(1.0),
            self.selection.value,
            window,
        );

        div()
            .id("color-picker-wheel")
            .relative()
            .size(px(COLOR_WHEEL_SIZE))
            .rounded_full()
            .overflow_hidden()
            .border_1()
            .border_color(cx.theme().colors.border)
            .shadow_md()
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_wheel_mouse_down))
            .on_mouse_move(cx.listener(Self::on_wheel_mouse_move))
            .child(
                canvas(
                    move |bounds, _, cx| {
                        entity.update(cx, |this, _| {
                            this.wheel_bounds = bounds;
                        });
                    },
                    move |bounds, _, window, _| {
                        let _ = window.paint_image(
                            bounds,
                            bounds,
                            gpui::Corners::default(),
                            wheel_image.clone(),
                            0,
                            false,
                        );
                    },
                )
                .absolute()
                .size_full(),
            )
            .child(
                div()
                    .absolute()
                    .left(px(handle_left))
                    .top(px(handle_top))
                    .size(px(COLOR_WHEEL_HANDLE_SIZE))
                    .rounded_full()
                    .border_2()
                    .border_color(cx.theme().colors.popover_foreground)
                    .shadow_md()
                    .child(
                        div()
                            .absolute()
                            .inset(px(3.0))
                            .rounded_full()
                            .bg(rgb(self.selection.rgb())),
                    ),
            )
    }

    fn render_brightness_slider(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().clone();
        let theme = cx.theme().colors;
        let base_color = hsv_to_rgb(self.selection.hue, self.selection.saturation, 1.0);
        let handle_left = brightness_handle_left(self.brightness_bounds, self.selection.value);

        div()
            .id("color-picker-brightness")
            .relative()
            .h(px(16.0))
            .w_full()
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(Self::on_brightness_mouse_down),
            )
            .on_mouse_move(cx.listener(Self::on_brightness_mouse_move))
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(5.0))
                    .h(px(BRIGHTNESS_TRACK_HEIGHT))
                    .rounded_full()
                    .bg(linear_gradient(
                        90.0,
                        linear_color_stop(rgb(0x000000), 0.0),
                        linear_color_stop(rgb(base_color), 1.0),
                    )
                    .color_space(ColorSpace::Srgb))
                    .child(
                        canvas(
                            move |bounds, _, cx| {
                                entity.update(cx, |this, _| {
                                    this.brightness_bounds = bounds;
                                });
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .left(px(handle_left))
                    .top(px(1.0))
                    .size(px(BRIGHTNESS_HANDLE_SIZE))
                    .rounded_full()
                    .border_2()
                    .border_color(theme.popover_foreground)
                    .bg(rgb(self.selection.rgb()))
                    .shadow_md(),
            )
    }
}

impl Render for ColorPickerPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let translator = i18n::translator(cx);
        let focus_handle = cx.focus_handle();

        div()
            .id("color-picker-panel")
            .w(px(COLOR_PICKER_WIDTH))
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.foreground)
            .track_focus(&focus_handle)
            .child(dialog_header(
                translator.translate("color_picker.title"),
                theme,
            ))
            .child(dialog_body(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .gap(px(12.0))
                            .child(self.render_color_wheel(window, cx))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .h(px(COLOR_WHEEL_SIZE))
                                    .justify_between()
                                    .flex_1()
                                    .child(field(
                                        translator.translate("color_picker.brightness"),
                                        self.render_brightness_slider(cx),
                                        theme,
                                    ))
                                    .child(field(
                                        translator.translate("color_picker.hex"),
                                        div()
                                            .h(px(INPUT_HEIGHT))
                                            .child(Input::new(&self.hex_input).small().w_full()),
                                        theme,
                                    ))
                                    .child(
                                        div().flex().gap(px(6.0)).children([
                                            rgb_input_column(
                                                translator.translate("color_picker.channel.red"),
                                                self.red_input.clone(),
                                                theme,
                                            )
                                            .into_any_element(),
                                            rgb_input_column(
                                                translator.translate("color_picker.channel.green"),
                                                self.green_input.clone(),
                                                theme,
                                            )
                                            .into_any_element(),
                                            rgb_input_column(
                                                translator.translate("color_picker.channel.blue"),
                                                self.blue_input.clone(),
                                                theme,
                                            )
                                            .into_any_element(),
                                        ]),
                                    ),
                            ),
                    )
                    // 恢复默认单独一行，避免页脚按钮过多时挤出窗口。
                    .children(self.reset_label.clone().map(|label| {
                        div().flex().child(
                            dialog_button("color-picker-reset", label).ghost().on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.finish(None, window, cx);
                                }),
                            ),
                        )
                    }))
                    .child(dialog_footer(
                        vec![
                            dialog_button(
                                "color-picker-random",
                                translator.translate("color_picker.random"),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.random_color(window, cx);
                            }))
                            .into_any_element(),
                            dialog_button(
                                "color-picker-copy-hex",
                                translator.translate("color_picker.copy_hex"),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.copy_hex(cx)))
                            .into_any_element(),
                            dialog_button(
                                "color-picker-cancel",
                                translator.translate("main.form.cancel"),
                            )
                            .on_click(|_, window: &mut Window, _| window.remove_window())
                            .into_any_element(),
                            dialog_primary_button(
                                "color-picker-done",
                                translator.translate("color_picker.done"),
                            )
                            .on_click(cx.listener(|this, _, window, cx| this.done(window, cx)))
                            .into_any_element(),
                        ],
                        theme,
                    )),
            ))
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
    }
}

fn field(
    label: impl Into<gpui::SharedString>,
    control: impl IntoElement,
    theme: gpui_component::ThemeColor,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .child(
            div()
                .text_size(ui_rem(11.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.muted_foreground)
                .child(label.into()),
        )
        .child(control)
}

fn rgb_input_column(
    label: impl Into<gpui::SharedString>,
    input: Entity<InputState>,
    theme: gpui_component::ThemeColor,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .flex_1()
        .child(
            div()
                .text_size(ui_rem(11.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.muted_foreground)
                .child(label.into()),
        )
        .child(
            div()
                .h(px(INPUT_HEIGHT))
                .child(Input::new(&input).small().w_full()),
        )
}

fn set_input_value_if_changed(
    input: &Entity<InputState>,
    value: String,
    window: &mut Window,
    cx: &mut Context<ColorPickerPanel>,
) {
    input.update(cx, |input, cx| {
        if input.value().as_ref() != value {
            input.set_value(value, window, cx);
        }
    });
}

#[derive(Clone, Copy, Debug)]
struct ColorSelection {
    hue: f32,
    saturation: f32,
    value: f32,
}

impl ColorSelection {
    fn from_rgb(color: u32, fallback_hue: Option<f32>) -> Self {
        rgb_to_hsv(color, fallback_hue)
    }

    fn rgb(self) -> u32 {
        hsv_to_rgb(self.hue, self.saturation, self.value)
    }

    fn red(self) -> u8 {
        ((self.rgb() >> 16) & 0xff) as u8
    }

    fn green(self) -> u8 {
        ((self.rgb() >> 8) & 0xff) as u8
    }

    fn blue(self) -> u8 {
        (self.rgb() & 0xff) as u8
    }
}

fn rgb_to_hsv(color: u32, fallback_hue: Option<f32>) -> ColorSelection {
    let red = ((color >> 16) & 0xff) as f32 / 255.0;
    let green = ((color >> 8) & 0xff) as f32 / 255.0;
    let blue = (color & 0xff) as f32 / 255.0;
    let max = red.max(green).max(blue);
    let min = red.min(green).min(blue);
    let delta = max - min;

    let hue = if delta <= f32::EPSILON {
        fallback_hue.unwrap_or(0.0)
    } else if (max - red).abs() <= f32::EPSILON {
        ((green - blue) / delta).rem_euclid(6.0) / 6.0
    } else if (max - green).abs() <= f32::EPSILON {
        ((blue - red) / delta + 2.0) / 6.0
    } else {
        ((red - green) / delta + 4.0) / 6.0
    };
    let saturation = if max <= f32::EPSILON {
        0.0
    } else {
        delta / max
    };

    ColorSelection {
        hue: hue.clamp(0.0, 1.0),
        saturation: saturation.clamp(0.0, 1.0),
        value: max.clamp(0.0, 1.0),
    }
}

fn hsv_to_rgb(hue: f32, saturation: f32, value: f32) -> u32 {
    let hue = hue.rem_euclid(1.0) * 6.0;
    let saturation = saturation.clamp(0.0, 1.0);
    let value = value.clamp(0.0, 1.0);
    let chroma = value * saturation;
    let x = chroma * (1.0 - (hue % 2.0 - 1.0).abs());
    let m = value - chroma;
    let (red, green, blue) = match hue.floor() as i32 {
        0 => (chroma, x, 0.0),
        1 => (x, chroma, 0.0),
        2 => (0.0, chroma, x),
        3 => (0.0, x, chroma),
        4 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };

    let red = ((red + m) * 255.0).round().clamp(0.0, 255.0) as u32;
    let green = ((green + m) * 255.0).round().clamp(0.0, 255.0) as u32;
    let blue = ((blue + m) * 255.0).round().clamp(0.0, 255.0) as u32;
    (red << 16) | (green << 8) | blue
}

fn parse_hex_color(value: &str) -> Option<u32> {
    let value = value.trim().trim_start_matches('#');
    match value.len() {
        3 => {
            let mut expanded = String::with_capacity(6);
            for ch in value.chars() {
                if !ch.is_ascii_hexdigit() {
                    return None;
                }
                expanded.push(ch);
                expanded.push(ch);
            }
            u32::from_str_radix(&expanded, 16).ok()
        }
        6 if value.chars().all(|ch| ch.is_ascii_hexdigit()) => u32::from_str_radix(value, 16).ok(),
        _ => None,
    }
}

fn parse_rgb_input(value: &str) -> Option<u8> {
    let value = value.trim();
    if value.is_empty() || value.len() > 3 || !value.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    value.parse::<u8>().ok()
}

fn color_wheel_position(bounds: Bounds<Pixels>, position: Point<Pixels>) -> Option<(f32, f32)> {
    let width = bounds.size.width.as_f32();
    let height = bounds.size.height.as_f32();
    let size = width.min(height);
    let radius = (size / 2.0 - COLOR_WHEEL_HANDLE_SIZE / 2.0).max(1.0);
    if radius <= 0.0 {
        return None;
    }

    let center_x = bounds.left().as_f32() + width / 2.0;
    let center_y = bounds.top().as_f32() + height / 2.0;
    let dx = position.x.as_f32() - center_x;
    let dy = position.y.as_f32() - center_y;
    let distance = (dx * dx + dy * dy).sqrt();
    let hue = dy.atan2(dx).rem_euclid(TAU) / TAU;
    let saturation = (distance / radius).clamp(0.0, 1.0);

    Some((hue, saturation))
}

fn wheel_handle_position(bounds: Bounds<Pixels>, selection: ColorSelection) -> (f32, f32) {
    let size = bounds.size.width.as_f32().min(bounds.size.height.as_f32());
    if size <= 0.0 {
        let center = COLOR_WHEEL_SIZE / 2.0 - COLOR_WHEEL_HANDLE_SIZE / 2.0;
        return (center, center);
    }

    let center = size / 2.0;
    let radius = (center - COLOR_WHEEL_HANDLE_SIZE / 2.0).max(1.0);
    let angle = selection.hue * TAU;
    let distance = selection.saturation * radius;
    let x = center + angle.cos() * distance - COLOR_WHEEL_HANDLE_SIZE / 2.0;
    let y = center + angle.sin() * distance - COLOR_WHEEL_HANDLE_SIZE / 2.0;
    (x, y)
}

fn brightness_handle_left(bounds: Bounds<Pixels>, value: f32) -> f32 {
    let width = bounds.size.width.as_f32();
    if width <= 0.0 {
        return COLOR_PICKER_WIDTH - DIALOG_PADDING * 2.0 - BRIGHTNESS_HANDLE_SIZE;
    }

    (value.clamp(0.0, 1.0) * width - BRIGHTNESS_HANDLE_SIZE / 2.0).clamp(
        -BRIGHTNESS_HANDLE_SIZE / 2.0,
        width - BRIGHTNESS_HANDLE_SIZE / 2.0,
    )
}

#[derive(Default)]
struct ColorWheelImageCache {
    key: Option<ColorWheelImageKey>,
    image: Option<Arc<RenderImage>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ColorWheelImageKey {
    physical_size: u32,
    value_milli: u16,
}

impl ColorWheelImageCache {
    fn image(
        &mut self,
        logical_size: f32,
        scale_factor: f32,
        value: f32,
        window: &mut Window,
    ) -> Arc<RenderImage> {
        let scale_factor = scale_factor.max(1.0);
        let physical_size = (logical_size * scale_factor).round().max(1.0) as u32;
        let value_milli = (value.clamp(0.0, 1.0) * 1000.0).round() as u16;
        let key = ColorWheelImageKey {
            physical_size,
            value_milli,
        };

        if self.key != Some(key) {
            if let Some(image) = self.image.take() {
                let _ = window.drop_image(image);
            }
            self.image = Some(create_color_wheel_image(
                physical_size,
                scale_factor,
                value_milli as f32 / 1000.0,
            ));
            self.key = Some(key);
        }

        self.image
            .as_ref()
            .expect("色环缓存必须在生成后存在")
            .clone()
    }
}

fn create_color_wheel_image(physical_size: u32, scale_factor: f32, value: f32) -> Arc<RenderImage> {
    let mut bgra = vec![0; physical_size as usize * physical_size as usize * 4];
    for y in 0..physical_size {
        for x in 0..physical_size {
            let pixel = color_wheel_pixel_bgra(physical_size, scale_factor, value, x, y);
            let index = (y as usize * physical_size as usize + x as usize) * 4;
            bgra[index..index + 4].copy_from_slice(&pixel);
        }
    }

    let image_buffer = RgbaImage::from_raw(physical_size, physical_size, bgra)
        .expect("色环图像缓冲尺寸必须与像素数据匹配");
    Arc::new(RenderImage::new(vec![Frame::new(image_buffer)]))
}

fn color_wheel_pixel_bgra(
    physical_size: u32,
    scale_factor: f32,
    value: f32,
    x: u32,
    y: u32,
) -> [u8; 4] {
    let size = physical_size as f32;
    let center = size / 2.0;
    let radius = (center - 2.0 * scale_factor.max(1.0)).max(1.0);
    let sample_x = x as f32 + 0.5;
    let sample_y = y as f32 + 0.5;
    let dx = sample_x - center;
    let dy = sample_y - center;
    let distance = (dx * dx + dy * dy).sqrt();
    let edge_alpha = ((radius - distance) / (2.0 * scale_factor.max(1.0))).clamp(0.0, 1.0);
    if edge_alpha <= 0.0 {
        return [0, 0, 0, 0];
    }

    let hue = dy.atan2(dx).rem_euclid(TAU) / TAU;
    let saturation = (distance / radius).clamp(0.0, 1.0);
    let color = hsv_to_rgb(hue, saturation, value);
    let red = ((color >> 16) & 0xff) as u8;
    let green = ((color >> 8) & 0xff) as u8;
    let blue = (color & 0xff) as u8;

    [
        blue,
        green,
        red,
        (edge_alpha * 255.0).round().clamp(0.0, 255.0) as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_color_parser_accepts_three_and_six_digit_values() {
        assert_eq!(parse_hex_color("#E645FF"), Some(0xe645ff));
        assert_eq!(parse_hex_color("f09"), Some(0xff0099));
        assert_eq!(parse_hex_color("#xyz"), None);
    }

    #[test]
    fn rgb_hsv_conversion_preserves_color_value() {
        let selection = ColorSelection::from_rgb(0xe645ff, None);
        assert_eq!(selection.rgb(), 0xe645ff);
    }

    #[test]
    fn rgb_input_is_limited_to_single_channel_range() {
        assert_eq!(parse_rgb_input("255"), Some(255));
        assert_eq!(parse_rgb_input("256"), None);
        assert_eq!(parse_rgb_input("-1"), None);
    }

    #[test]
    fn color_wheel_edge_pixels_are_transparent() {
        assert_eq!(color_wheel_pixel_bgra(21, 1.0, 1.0, 0, 0)[3], 0);
    }
}
