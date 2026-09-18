//! A single display-column layout shared by rendering and hit testing.
use unicode_width::UnicodeWidthChar;

#[derive(Debug, Clone)]
pub struct InputLine {
    pub text: String,
    pub start: usize,
    pub end: usize,
}
#[derive(Debug, Clone)]
pub struct InputLayout {
    pub lines: Vec<InputLine>,
    pub cursor_row: usize,
    pub cursor_column: usize,
}
impl InputLayout {
    pub fn new(text: &str, cursor: usize, width: u16) -> Self {
        let width = usize::from(width.max(1));
        let mut lines = Vec::new();
        let mut line = String::new();
        let mut column = 0;
        let mut start = 0;
        let total = text.chars().count();
        let cursor = cursor.min(total);
        let mut cursor_pos = None;
        for (i, ch) in text.chars().enumerate() {
            let columns = ch.width().unwrap_or(0);
            if column + columns > width && !line.is_empty() {
                lines.push(InputLine {
                    text: std::mem::take(&mut line),
                    start,
                    end: i,
                });
                column = 0;
                start = i;
            }
            if i == cursor {
                cursor_pos = Some((lines.len(), column));
            }
            line.push(ch);
            column += columns;
        }
        lines.push(InputLine {
            text: line,
            start,
            end: total,
        });
        if cursor == total {
            if column >= width && total > 0 {
                lines.push(InputLine {
                    text: String::new(),
                    start: total,
                    end: total,
                });
                column = 0;
            }
            cursor_pos = Some((lines.len() - 1, column));
        }
        let (cursor_row, cursor_column) = cursor_pos.unwrap_or((0, 0));
        Self {
            lines,
            cursor_row,
            cursor_column,
        }
    }
}
