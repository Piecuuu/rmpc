// This is a "fork" of ratatui's Tabs widget made to remove the spacing between tabs

// The MIT License (MIT)
//
// Copyright (c) 2016-2022 Florian Dehau
// Copyright (c) 2023 The Ratatui Developers
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Style, Styled},
    symbols,
    text::{Line, Span},
    widgets::{Block, Widget},
};

/// A widget to display available tabs in a multiple panels context.
///
/// # Examples
///
/// ```
/// # use ratatui::widgets::{Block, Borders, Tabs};
/// # use ratatui::style::{Style, Color};
/// # use ratatui::text::{Line};
/// # use ratatui::symbols::{DOT};
/// let titles = ["Tab1", "Tab2", "Tab3", "Tab4"].iter().cloned().map(Line::from).collect();
/// Tabs::new(titles)
///     .block(Block::default().title("Tabs").borders(Borders::ALL))
///     .style(Style::default().fg(Color::White))
///     .highlight_style(Style::default().fg(Color::Yellow))
///     .divider(DOT);
/// ```
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Tabs<'a> {
    /// A block to wrap this widget in if necessary
    block: Option<Block<'a>>,
    /// One title for each tab
    titles: Vec<Line<'a>>,
    /// The index of the selected tabs
    selected: usize,
    /// The style used to draw the text
    style: Style,
    /// Style to apply to the selected item
    highlight_style: Style,
    /// Tab divider
    divider: Span<'a>,
}

#[allow(unused)]
impl<'a> Tabs<'a> {
    pub fn new<T>(titles: Vec<T>) -> Tabs<'a>
    where
        T: Into<Line<'a>>,
    {
        Tabs {
            block: None,
            titles: titles.into_iter().map(Into::into).collect(),
            selected: 0,
            style: Style::default(),
            highlight_style: Style::default(),
            divider: Span::raw(symbols::line::VERTICAL),
        }
    }

    pub fn block(mut self, block: Block<'a>) -> Tabs<'a> {
        self.block = Some(block);
        self
    }

    pub fn select(mut self, selected: usize) -> Tabs<'a> {
        self.selected = selected;
        self
    }

    pub fn style(mut self, style: Style) -> Tabs<'a> {
        self.style = style;
        self
    }

    pub fn highlight_style(mut self, style: Style) -> Tabs<'a> {
        self.highlight_style = style;
        self
    }

    pub fn divider<T>(mut self, divider: T) -> Tabs<'a>
    where
        T: Into<Span<'a>>,
    {
        self.divider = divider.into();
        self
    }
}

impl<'a> Styled for Tabs<'a> {
    type Item = Tabs<'a>;

    fn style(&self) -> Style {
        self.style
    }

    fn set_style(self, style: Style) -> Self::Item {
        self.style(style)
    }
}

impl<'a> Widget for Tabs<'a> {
    fn render(mut self, area: Rect, buf: &mut Buffer) {
        buf.set_style(area, self.style);
        let tabs_area = match self.block.take() {
            Some(b) => {
                let inner_area = b.inner(area);
                b.render(area, buf);
                inner_area
            }
            None => area,
        };

        if tabs_area.height < 1 {
            return;
        }

        let mut x = tabs_area.left();
        let titles_length = self.titles.len();
        for (i, title) in self.titles.into_iter().enumerate() {
            let last_title = titles_length - 1 == i;
            let remaining_width = tabs_area.right().saturating_sub(x);
            if remaining_width == 0 {
                break;
            }
            let pos = buf.set_line(x, tabs_area.top(), &title, remaining_width);
            if i == self.selected {
                buf.set_style(
                    Rect {
                        x,
                        y: tabs_area.top(),
                        width: pos.0.saturating_sub(x),
                        height: 1,
                    },
                    self.highlight_style,
                );
            }
            x = pos.0.saturating_add(1);
            let remaining_width = tabs_area.right().saturating_sub(x);
            if remaining_width == 0 || last_title {
                break;
            }
            let pos = buf.set_span(x - 1, tabs_area.top(), &self.divider, self.divider.width() as u16);
            x = pos.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Modifier, Stylize};

    #[test]
    fn can_be_stylized() {
        assert_eq!(
            Tabs::new(vec![""]).black().on_white().bold().not_italic().style,
            Style::default()
                .fg(Color::Black)
                .bg(Color::White)
                .add_modifier(Modifier::BOLD)
                .remove_modifier(Modifier::ITALIC)
        );
    }
}
