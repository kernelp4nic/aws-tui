//! Emit adjacent cells of the same style as one text run. In particular, avoid
//! cursor commands immediately after a flag: some terminal shapers finalize the
//! regional indicators separately when text is interrupted by cursor movement.
use ratatui::{
    backend::{Backend, ClearType, WindowSize},
    buffer::{Cell, CellWidth},
    layout::{Position, Size},
};

pub struct TextRuns<'a, B>(pub &'a mut B);

fn runs<'a>(content: impl Iterator<Item = (u16, u16, &'a Cell)>) -> Vec<(u16, u16, Cell)> {
    let mut output: Vec<(u16, u16, Cell)> = Vec::new();
    let mut end = None;
    for (x, y, cell) in content {
        let next = Some((x.saturating_add(cell.cell_width()), y));
        if let Some((_, _, previous)) = output.last_mut() {
            if end == Some((x, y)) && previous.style() == cell.style() {
                let mut text = previous.symbol().to_owned();
                text.push_str(cell.symbol());
                previous.set_symbol(&text);
                end = next;
                continue;
            }
        }
        output.push((x, y, cell.clone()));
        end = next;
    }
    output
}
impl<B: Backend> Backend for TextRuns<'_, B> {
    type Error = B::Error;
    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let output = runs(content);
        self.0
            .draw(output.iter().map(|(x, y, cell)| (*x, *y, cell)))
    }
    fn append_lines(&mut self, n: u16) -> Result<(), Self::Error> {
        self.0.append_lines(n)
    }
    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.0.hide_cursor()
    }
    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.0.show_cursor()
    }
    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.0.get_cursor_position()
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, p: P) -> Result<(), Self::Error> {
        self.0.set_cursor_position(p)
    }
    fn clear(&mut self) -> Result<(), Self::Error> {
        self.0.clear()
    }
    fn clear_region(&mut self, c: ClearType) -> Result<(), Self::Error> {
        self.0.clear_region(c)
    }
    fn size(&self) -> Result<Size, Self::Error> {
        self.0.size()
    }
    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.0.window_size()
    }
    fn flush(&mut self) -> Result<(), Self::Error> {
        self.0.flush()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::CrosstermBackend, buffer::Buffer, layout::Rect};
    #[test]
    fn flag_and_surrounding_text_are_emitted_without_cursor_sequences() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 30, 1));
        buffer.set_string(0, 0, "[web 🇺🇾] running", ratatui::style::Style::default());
        let empty = Buffer::empty(buffer.area);
        let mut output = Vec::<u8>::new();
        let mut backend = CrosstermBackend::new(&mut output);
        TextRuns(&mut backend)
            .draw(empty.diff(&buffer).into_iter())
            .unwrap();
        let bytes = String::from_utf8(output).unwrap();
        assert!(bytes.contains("🇺🇾]"), "Unexpected output: {bytes:?}");
    }
    #[test]
    fn runs_preserve_style_changes_and_gaps() {
        let a = Cell::new("a");
        let b = Cell::new("b");
        let mut c = Cell::new("c");
        c.set_fg(ratatui::style::Color::Red);
        let output = runs([(0, 0, &a), (1, 0, &b), (2, 0, &c), (5, 0, &a)].into_iter());
        assert_eq!(output.len(), 3);
        assert_eq!(output[0].2.symbol(), "ab");
        assert_eq!(output[1].0, 2);
        assert_eq!(output[2].0, 5);
    }
}
