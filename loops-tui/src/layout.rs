use ratatui::layout::{Constraint, Direction, Layout, Rect};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Density {
  TooSmall,
  Narrow,
  Medium,
  Wide,
}

pub fn density(area: Rect) -> Density {
  if area.width < 42 || area.height < 12 {
    Density::TooSmall
  } else if area.width < 72 {
    Density::Narrow
  } else if area.width < 110 {
    Density::Medium
  } else {
    Density::Wide
  }
}

pub fn shell(area: Rect) -> (Rect, Rect, Rect) {
  let rows = Layout::default()
    .direction(Direction::Vertical)
    .constraints([
      Constraint::Length(2),
      Constraint::Min(1),
      Constraint::Length(1),
    ])
    .split(area);
  (rows[0], rows[1], rows[2])
}

pub fn split_main(area: Rect, kind: Density) -> (Rect, Option<Rect>) {
  match kind {
    Density::Wide => {
      let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
          Constraint::Percentage(66),
          Constraint::Length(2),
          Constraint::Percentage(34),
        ])
        .split(area);
      (columns[0], Some(columns[2]))
    }
    _ => (area, None),
  }
}

pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
  let horizontal = Layout::default()
    .direction(Direction::Horizontal)
    .constraints([
      Constraint::Percentage(50),
      Constraint::Length(width.min(area.width)),
      Constraint::Percentage(50),
    ])
    .split(area);
  let vertical = Layout::default()
    .direction(Direction::Vertical)
    .constraints([
      Constraint::Percentage(50),
      Constraint::Length(height.min(area.height)),
      Constraint::Percentage(50),
    ])
    .split(horizontal[1]);
  vertical[1]
}
