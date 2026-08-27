//! Mouse hit-testing: map a screen cell to the region under the pointer.

use crate::ui::{GAP_ROWS, HORIZONTAL_PAD, SEPARATOR_GAP, SIDEBAR_WIDTH};

/// What the mouse pointer (or wheel) is over.
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
pub fn hit(x: u16, y: u16, screen_w: u16, screen_h: u16, composer_rows: u16) -> MouseTarget {
    if y >= screen_h || x >= screen_w {
        return MouseTarget::None;
    }
    if x < SIDEBAR_WIDTH {
        // Sidebar rows render directly on screen rows 0.. (ui::inset pads
        // columns only); row bounds are validated by the caller against
        // sidebar_rows().len().
        return MouseTarget::SidebarRow(usize::from(y));
    }
    let main_x = SIDEBAR_WIDTH + SEPARATOR_GAP;
    if x < main_x {
        return MouseTarget::None; // the │ separator and its 1-col gap
    }
    let composer_top = screen_h.saturating_sub(GAP_ROWS + composer_rows);
    // The composer owns [composer_top, composer_top + composer_rows) — the
    // GAP_ROWS below it (fade + blank) are not part of it.
    if composer_rows > 0 && y >= composer_top && y < composer_top + composer_rows {
        return MouseTarget::Composer;
    }
    // Message pane: the main column inset by HORIZONTAL_PAD each side, from
    // the top of the screen down to the composer (or the gap on welcome).
    let right_edge = screen_w.saturating_sub(HORIZONTAL_PAD);
    if x >= main_x + HORIZONTAL_PAD && x < right_edge && y < composer_top {
        return MouseTarget::MessageRow(y);
    }
    MouseTarget::None
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn welcome_page_without_composer_extends_the_pane_to_the_gap() {
        // composer_rows = 0: pane rows go down to screen_h - GAP_ROWS.
        assert_eq!(hit(26, 5, W, H, 0), MouseTarget::MessageRow(5));
        assert_eq!(hit(26, 8, W, H, 0), MouseTarget::None); // gap rows
        assert_eq!(hit(26, 9, W, H, 0), MouseTarget::None);
    }
}
