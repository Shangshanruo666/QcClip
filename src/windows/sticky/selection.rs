/// 拆词模式下的词块选择：点击切换、拖动连续涂选、Shift 连选。
#[derive(Clone, Debug, Default)]
pub(super) struct TokenSelection {
    selected: Vec<bool>,
    drag: Option<DragSelection>,
    /// 上一次点击的词块，作为 Shift 连选的起点。
    last_anchor: Option<usize>,
}

#[derive(Clone, Debug)]
struct DragSelection {
    anchor: usize,
    current: usize,
    /// 拖动经过的词块统一设为该值；起点原本已选中时拖动表现为取消选择。
    value: bool,
    base: Vec<bool>,
}

impl TokenSelection {
    pub(super) fn new(len: usize) -> Self {
        Self {
            selected: vec![false; len],
            ..Default::default()
        }
    }

    pub(super) fn len(&self) -> usize {
        self.selected.len()
    }

    pub(super) fn is_selected(&self, index: usize) -> bool {
        self.selected.get(index).copied().unwrap_or(false)
    }

    pub(super) fn selected_count(&self) -> usize {
        self.selected.iter().filter(|selected| **selected).count()
    }

    pub(super) fn selected_indices(&self) -> impl Iterator<Item = usize> + '_ {
        self.selected
            .iter()
            .enumerate()
            .filter_map(|(index, selected)| selected.then_some(index))
    }

    pub(super) fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub(super) fn press(&mut self, index: usize, extend_range: bool) {
        if index >= self.selected.len() {
            return;
        }
        if extend_range && let Some(anchor) = self.last_anchor {
            let (start, end) = ordered(anchor, index);
            for selected in &mut self.selected[start..=end] {
                *selected = true;
            }
            self.last_anchor = Some(index);
            return;
        }
        let value = !self.selected[index];
        self.drag = Some(DragSelection {
            anchor: index,
            current: index,
            value,
            base: self.selected.clone(),
        });
        self.selected[index] = value;
    }

    /// 拖动经过新的词块时返回 true，调用方据此刷新界面。
    pub(super) fn drag_to(&mut self, index: usize) -> bool {
        let Some(drag) = self.drag.as_mut() else {
            return false;
        };
        if index >= self.selected.len() || drag.current == index {
            return false;
        }
        drag.current = index;
        self.selected.clone_from(&drag.base);
        let (start, end) = ordered(drag.anchor, index);
        for selected in &mut self.selected[start..=end] {
            *selected = drag.value;
        }
        true
    }

    pub(super) fn release(&mut self) {
        if let Some(drag) = self.drag.take() {
            self.last_anchor = Some(drag.current);
        }
    }

    pub(super) fn select_all(&mut self) {
        self.selected.fill(true);
    }

    pub(super) fn invert(&mut self) {
        for selected in &mut self.selected {
            *selected = !*selected;
        }
    }

    pub(super) fn clear(&mut self) {
        self.selected.fill(false);
        self.last_anchor = None;
    }
}

fn ordered(first: usize, second: usize) -> (usize, usize) {
    if first <= second {
        (first, second)
    } else {
        (second, first)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected(selection: &TokenSelection) -> Vec<usize> {
        selection.selected_indices().collect()
    }

    #[test]
    fn click_toggles_single_token() {
        let mut selection = TokenSelection::new(4);
        selection.press(1, false);
        selection.release();
        assert_eq!(selected(&selection), [1]);
        selection.press(1, false);
        selection.release();
        assert!(selected(&selection).is_empty());
    }

    #[test]
    fn drag_paints_range_and_can_shrink_back() {
        let mut selection = TokenSelection::new(6);
        selection.press(4, false);
        selection.release();
        selection.press(1, false);
        assert!(selection.drag_to(3));
        assert_eq!(selected(&selection), [1, 2, 3, 4]);
        assert!(selection.drag_to(2));
        assert_eq!(selected(&selection), [1, 2, 4]);
        assert!(!selection.drag_to(2));
        selection.release();
        assert!(!selection.is_dragging());
    }

    #[test]
    fn dragging_from_selected_token_deselects() {
        let mut selection = TokenSelection::new(5);
        selection.select_all();
        selection.press(3, false);
        selection.drag_to(1);
        selection.release();
        assert_eq!(selected(&selection), [0, 4]);
    }

    #[test]
    fn shift_extends_from_last_anchor() {
        let mut selection = TokenSelection::new(8);
        selection.press(2, false);
        selection.release();
        selection.press(5, true);
        assert_eq!(selected(&selection), [2, 3, 4, 5]);
        selection.invert();
        assert_eq!(selected(&selection), [0, 1, 6, 7]);
        selection.clear();
        assert_eq!(selection.selected_count(), 0);
    }
}
