//! Mouse hit-testing: map a screen cell to the region under the pointer.

use super::render::ScreenGeometry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseTarget {
    /// Visible row `i` of the sidebar (0 = first server header).
    SidebarRow(usize),
    /// Row `r` of the message pane, as an absolute screen row (the pane
    /// starts at screen row 0). Scroll offset is NOT applied here.
    MessageRow(u16),
    /// Inside the message pane but not on a message row (separator/blank).
    MessageBlank,
    /// The composer region (panel, accent column, right gap).
    Composer,
    /// Anything else: separator column, paddings, gap rows.
    None,
}

/// Map screen cell `(x, y)` to the region under it.
///
/// `composer_rows` is the composer's height in rows (`ui::composer_height`
/// for the current input); 0 on the welcome page, which has no composer.
pub fn hit(x: u16, y: u16, g: &ScreenGeometry) -> MouseTarget {
    let point = ratatui::layout::Position::new(x, y);
    if g.sidebar.contains(point) {
        return MouseTarget::SidebarRow(usize::from(y - g.sidebar.y));
    }
    if g.composer.contains(point) {
        return MouseTarget::Composer;
    }
    if g.messages.contains(point) {
        return MouseTarget::MessageRow(y - g.messages.y);
    }
    MouseTarget::None
}

#[cfg(test)]
mod tests {
    use super::*;
    fn hit(x: u16, y: u16, w: u16, h: u16, composer_rows: u16) -> MouseTarget {
        let g = crate::tui::render::geometry(w, h, "", 0, composer_rows > 0);
        super::hit(x, y, &g)
    }

    // A 50x10 screen with a 4-row composer: the message pane owns rows
    // 0..=3 of the main column; composer rows 4..=7; gap rows 8..=9.
    const W: u16 = 50;
    const H: u16 = 10;
    const COMPOSER: u16 = 4;

    #[test]
    fn sidebar_rows_map_directly_to_screen_rows() {
        // Arrange / Act / Assert: sidebar row i renders on screen row i
        // (ui::inset pads columns only).
        assert_eq!(hit(0, 0, W, H, COMPOSER), MouseTarget::SidebarRow(0));
        assert_eq!(hit(21, 2, W, H, COMPOSER), MouseTarget::SidebarRow(2));
    }

    #[test]
    fn out_of_bounds_coordinates_are_none() {
        assert_eq!(hit(0, 10, W, H, COMPOSER), MouseTarget::None); // y == screen_h
        assert_eq!(hit(26, H, W, H, COMPOSER), MouseTarget::None);
        assert_eq!(hit(50, 1, W, H, COMPOSER), MouseTarget::None); // x == screen_w
    }

    #[test]
    fn separator_column_and_gap_are_none() {
        assert_eq!(hit(22, 1, W, H, COMPOSER), MouseTarget::None); // the │ column
        assert_eq!(hit(23, 5, W, H, COMPOSER), MouseTarget::None); // 1-col blank gap
    }

    #[test]
    fn message_pane_rows_map_to_their_screen_row() {
        // The pane's content spans x = 26..=47 (22 + 2 + 2 .. 50 - 2 - 1).
        assert_eq!(hit(26, 0, W, H, COMPOSER), MouseTarget::MessageRow(0));
        assert_eq!(hit(47, 3, W, H, COMPOSER), MouseTarget::MessageRow(3));
    }

    #[test]
    fn message_pane_horizontal_padding_is_none() {
        assert_eq!(hit(24, 1, W, H, COMPOSER), MouseTarget::None); // left pad
        assert_eq!(hit(25, 1, W, H, COMPOSER), MouseTarget::None);
        assert_eq!(hit(48, 1, W, H, COMPOSER), MouseTarget::None); // right pad
        assert_eq!(hit(49, 1, W, H, COMPOSER), MouseTarget::None);
    }

    #[test]
    fn composer_rows_are_composer_including_accent_and_right_gap() {
        assert_eq!(hit(24, 4, W, H, COMPOSER), MouseTarget::Composer); // ┃ accent col
        assert_eq!(hit(26, 7, W, H, COMPOSER), MouseTarget::Composer); // tips row
        assert_eq!(hit(49, 5, W, H, COMPOSER), MouseTarget::Composer); // right gap
    }

    #[test]
    fn rows_below_the_composer_are_none() {
        assert_eq!(hit(26, 8, W, H, COMPOSER), MouseTarget::None); // fade row
        assert_eq!(hit(26, 9, W, H, COMPOSER), MouseTarget::None);
    }

    #[test]
    fn welcome_page_uses_the_full_message_area() {
        // The welcome page has neither composer nor gap.
        assert_eq!(hit(26, 5, W, H, 0), MouseTarget::MessageRow(5));
        assert_eq!(hit(26, 8, W, H, 0), MouseTarget::MessageRow(8)); // full welcome pane
        assert_eq!(hit(26, 9, W, H, 0), MouseTarget::MessageRow(9));
    }
}
