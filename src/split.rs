//! Split tree layout management for Ghostty-compatible terminal panes.
//!
//! Supports arbitrary recursive horizontal and vertical splits, pane focus,
//! directional navigation (left/right/up/down), split zoom (maximize),
//! ratio resizing, and equalization.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    Vertical,   // Left | Right (vertical divider)
    Horizontal, // Top / Bottom (horizontal divider)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub col: usize,
    pub row: usize,
    pub cols: usize,
    pub rows: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneRect {
    pub id: u64,
    pub col: usize,
    pub row: usize,
    pub cols: usize,
    pub rows: usize,
    pub has_border_right: bool,
    pub has_border_bottom: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SplitNode {
    Leaf(u64),
    Split {
        direction: SplitDirection,
        ratio: f32, // fraction of space given to first child (0.1..=0.9, default 0.5)
        first: Box<SplitNode>,
        second: Box<SplitNode>,
    },
}

impl SplitNode {
    pub fn new_leaf(id: u64) -> Self {
        SplitNode::Leaf(id)
    }

    /// Recursively calculate bounding rectangles and divider borders for all leaf panes.
    pub fn compute_rects(&self, area: Rect, out: &mut Vec<PaneRect>) {
        match self {
            SplitNode::Leaf(id) => {
                out.push(PaneRect {
                    id: *id,
                    col: area.col,
                    row: area.row,
                    cols: area.cols,
                    rows: area.rows,
                    has_border_right: false,
                    has_border_bottom: false,
                });
            }
            SplitNode::Split {
                direction,
                ratio,
                first,
                second,
            } => match direction {
                SplitDirection::Vertical => {
                    let first_cols = ((area.cols as f32) * ratio)
                        .round()
                        .clamp(1.0, area.cols.saturating_sub(1) as f32)
                        as usize;
                    let second_cols = area.cols.saturating_sub(first_cols).max(1);

                    let first_area = Rect {
                        col: area.col,
                        row: area.row,
                        cols: first_cols,
                        rows: area.rows,
                    };
                    let second_area = Rect {
                        col: area.col + first_cols,
                        row: area.row,
                        cols: second_cols,
                        rows: area.rows,
                    };

                    let start_first = out.len();
                    first.compute_rects(first_area, out);
                    let end_first = out.len();
                    // Mark right border for leaves on the boundary between first and second
                    for p in &mut out[start_first..end_first] {
                        if p.col + p.cols == area.col + first_cols {
                            p.has_border_right = true;
                        }
                    }

                    second.compute_rects(second_area, out);
                }
                SplitDirection::Horizontal => {
                    let first_rows = ((area.rows as f32) * ratio)
                        .round()
                        .clamp(1.0, area.rows.saturating_sub(1) as f32)
                        as usize;
                    let second_rows = area.rows.saturating_sub(first_rows).max(1);

                    let first_area = Rect {
                        col: area.col,
                        row: area.row,
                        cols: area.cols,
                        rows: first_rows,
                    };
                    let second_area = Rect {
                        col: area.col,
                        row: area.row + first_rows,
                        cols: area.cols,
                        rows: second_rows,
                    };

                    let start_first = out.len();
                    first.compute_rects(first_area, out);
                    let end_first = out.len();
                    // Mark bottom border for leaves on the boundary between first and second
                    for p in &mut out[start_first..end_first] {
                        if p.row + p.rows == area.row + first_rows {
                            p.has_border_bottom = true;
                        }
                    }

                    second.compute_rects(second_area, out);
                }
            },
        }
    }

    /// Split a specific leaf pane into two. Returns true if target was found and split.
    pub fn split_pane(&mut self, target_id: u64, new_id: u64, direction: SplitDirection) -> bool {
        match self {
            SplitNode::Leaf(id) => {
                if *id == target_id {
                    *self = SplitNode::Split {
                        direction,
                        ratio: 0.5,
                        first: Box::new(SplitNode::Leaf(target_id)),
                        second: Box::new(SplitNode::Leaf(new_id)),
                    };
                    true
                } else {
                    false
                }
            }
            SplitNode::Split { first, second, .. } => {
                first.split_pane(target_id, new_id, direction)
                    || second.split_pane(target_id, new_id, direction)
            }
        }
    }

    /// Remove a pane from the tree. Returns true if removed, and the tree is simplified.
    pub fn remove_pane(&mut self, target_id: u64) -> bool {
        match self {
            SplitNode::Leaf(id) => *id == target_id,
            SplitNode::Split { first, second, .. } => {
                if let SplitNode::Leaf(id) = **first {
                    if id == target_id {
                        *self = *second.clone();
                        return true;
                    }
                }
                if let SplitNode::Leaf(id) = **second {
                    if id == target_id {
                        *self = *first.clone();
                        return true;
                    }
                }
                if first.remove_pane(target_id) {
                    return true;
                }
                if second.remove_pane(target_id) {
                    return true;
                }
                false
            }
        }
    }

    /// Collect all pane IDs in traversal order.
    pub fn all_pane_ids(&self, out: &mut Vec<u64>) {
        match self {
            SplitNode::Leaf(id) => out.push(*id),
            SplitNode::Split { first, second, .. } => {
                first.all_pane_ids(out);
                second.all_pane_ids(out);
            }
        }
    }

    pub fn contains(&self, target_id: u64) -> bool {
        match self {
            SplitNode::Leaf(id) => *id == target_id,
            SplitNode::Split { first, second, .. } => {
                first.contains(target_id) || second.contains(target_id)
            }
        }
    }

    /// Reset all split ratios to equal (0.5).
    pub fn equalize(&mut self) {
        if let SplitNode::Split {
            ratio,
            first,
            second,
            ..
        } = self
        {
            *ratio = 0.5;
            first.equalize();
            second.equalize();
        }
    }

    /// Adjust ratio of the parent split containing `target_id`.
    pub fn adjust_ratio(&mut self, target_id: u64, delta: f32) -> bool {
        match self {
            SplitNode::Leaf(_) => false,
            SplitNode::Split {
                ratio,
                first,
                second,
                ..
            } => {
                if first.contains(target_id) {
                    *ratio = (*ratio + delta).clamp(0.15, 0.85);
                    true
                } else if second.contains(target_id) {
                    *ratio = (*ratio - delta).clamp(0.15, 0.85);
                    true
                } else {
                    first.adjust_ratio(target_id, delta) || second.adjust_ratio(target_id, delta)
                }
            }
        }
    }
}

/// Find spatial neighbor pane in a given direction from the active pane.
pub fn find_directional_neighbor(
    rects: &[PaneRect],
    active_id: u64,
    direction: Direction,
) -> Option<u64> {
    let active = rects.iter().find(|p| p.id == active_id)?;
    let mut best: Option<(u64, usize)> = None;

    for p in rects {
        if p.id == active_id {
            continue;
        }
        match direction {
            Direction::Left => {
                if p.col + p.cols <= active.col {
                    // Check vertical overlap
                    let top = p.row.max(active.row);
                    let bot = (p.row + p.rows).min(active.row + active.rows);
                    if top < bot {
                        let dist = active.col - (p.col + p.cols);
                        if best.map_or(true, |(_, d)| dist < d) {
                            best = Some((p.id, dist));
                        }
                    }
                }
            }
            Direction::Right => {
                if p.col >= active.col + active.cols {
                    let top = p.row.max(active.row);
                    let bot = (p.row + p.rows).min(active.row + active.rows);
                    if top < bot {
                        let dist = p.col - (active.col + active.cols);
                        if best.map_or(true, |(_, d)| dist < d) {
                            best = Some((p.id, dist));
                        }
                    }
                }
            }
            Direction::Up => {
                if p.row + p.rows <= active.row {
                    let left = p.col.max(active.col);
                    let right = (p.col + p.cols).min(active.col + active.cols);
                    if left < right {
                        let dist = active.row - (p.row + p.rows);
                        if best.map_or(true, |(_, d)| dist < d) {
                            best = Some((p.id, dist));
                        }
                    }
                }
            }
            Direction::Down => {
                if p.row >= active.row + active.rows {
                    let left = p.col.max(active.col);
                    let right = (p.col + p.cols).min(active.col + active.cols);
                    if left < right {
                        let dist = p.row - (active.row + active.rows);
                        if best.map_or(true, |(_, d)| dist < d) {
                            best = Some((p.id, dist));
                        }
                    }
                }
            }
        }
    }

    best.map(|(id, _)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_leaf() {
        let node = SplitNode::new_leaf(1);
        let area = Rect {
            col: 0,
            row: 0,
            cols: 100,
            rows: 50,
        };
        let mut rects = Vec::new();
        node.compute_rects(area, &mut rects);
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].id, 1);
        assert_eq!(rects[0].cols, 100);
        assert_eq!(rects[0].rows, 50);
        assert!(!rects[0].has_border_right);
        assert!(!rects[0].has_border_bottom);
    }

    #[test]
    fn test_vertical_split() {
        let mut node = SplitNode::new_leaf(1);
        assert!(node.split_pane(1, 2, SplitDirection::Vertical));

        let area = Rect {
            col: 0,
            row: 0,
            cols: 100,
            rows: 50,
        };
        let mut rects = Vec::new();
        node.compute_rects(area, &mut rects);
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0].id, 1);
        assert_eq!(rects[0].col, 0);
        assert_eq!(rects[0].cols, 50);
        assert!(rects[0].has_border_right);
        assert!(!rects[0].has_border_bottom);

        assert_eq!(rects[1].id, 2);
        assert_eq!(rects[1].col, 50);
        assert_eq!(rects[1].cols, 50);
        assert!(!rects[1].has_border_right);

        // Directional navigation
        assert_eq!(
            find_directional_neighbor(&rects, 1, Direction::Right),
            Some(2)
        );
        assert_eq!(find_directional_neighbor(&rects, 2, Direction::Left), Some(1));
        assert_eq!(find_directional_neighbor(&rects, 1, Direction::Up), None);
    }

    #[test]
    fn test_horizontal_split() {
        let mut node = SplitNode::new_leaf(1);
        assert!(node.split_pane(1, 2, SplitDirection::Horizontal));

        let area = Rect {
            col: 0,
            row: 0,
            cols: 100,
            rows: 50,
        };
        let mut rects = Vec::new();
        node.compute_rects(area, &mut rects);
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0].id, 1);
        assert_eq!(rects[0].row, 0);
        assert_eq!(rects[0].rows, 25);
        assert!(rects[0].has_border_bottom);
        assert!(!rects[0].has_border_right);

        assert_eq!(rects[1].id, 2);
        assert_eq!(rects[1].row, 25);
        assert_eq!(rects[1].rows, 25);
        assert!(!rects[1].has_border_bottom);

        // Directional navigation
        assert_eq!(find_directional_neighbor(&rects, 1, Direction::Down), Some(2));
        assert_eq!(find_directional_neighbor(&rects, 2, Direction::Up), Some(1));
    }

    #[test]
    fn test_remove_pane() {
        let mut node = SplitNode::new_leaf(1);
        node.split_pane(1, 2, SplitDirection::Vertical);
        node.split_pane(2, 3, SplitDirection::Horizontal);

        assert!(node.remove_pane(2));
        let mut ids = Vec::new();
        node.all_pane_ids(&mut ids);
        assert_eq!(ids, vec![1, 3]);
    }

    #[test]
    fn test_remove_second_child_of_two_panes() {
        let mut node = SplitNode::new_leaf(1);
        node.split_pane(1, 2, SplitDirection::Vertical);

        // Crucial test: closing the second pane (id 2) must leave node as Leaf(1)
        assert!(node.remove_pane(2));
        assert_eq!(node, SplitNode::Leaf(1));
    }

    #[test]
    fn test_remove_first_child_of_two_panes() {
        let mut node = SplitNode::new_leaf(1);
        node.split_pane(1, 2, SplitDirection::Vertical);

        // Closing first pane (id 1) must leave node as Leaf(2)
        assert!(node.remove_pane(1));
        assert_eq!(node, SplitNode::Leaf(2));
    }
}
