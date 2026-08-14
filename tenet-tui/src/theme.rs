use ratatui::style::{Color, Modifier, Style};

// Semantic styles deliberately avoid a background assumption.  Color carries
// category; weight, glyphs, and reverse-video focus carry the meaning.
const ACCENT: Color = Color::Cyan;
const SUCCESS: Color = Color::Green;
const WARNING: Color = Color::Yellow;
const FAILURE: Color = Color::Red;
const MUTED: Color = Color::DarkGray;
const BORDER: Color = Color::Gray;

pub fn identity() -> Style {
  Style::default().add_modifier(Modifier::BOLD)
}

pub fn heading() -> Style {
  Style::default().add_modifier(Modifier::BOLD)
}

pub fn primary() -> Style {
  Style::default()
}

pub fn secondary() -> Style {
  Style::default()
}

pub fn muted() -> Style {
  Style::default().fg(MUTED).add_modifier(Modifier::DIM)
}

pub fn caption() -> Style {
  Style::default().fg(MUTED).add_modifier(Modifier::DIM)
}

pub fn code() -> Style {
  Style::default().add_modifier(Modifier::DIM)
}

pub fn accent() -> Style {
  Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

pub fn success() -> Style {
  Style::default().fg(SUCCESS).add_modifier(Modifier::BOLD)
}

pub fn warning() -> Style {
  Style::default().fg(WARNING).add_modifier(Modifier::BOLD)
}

pub fn failure() -> Style {
  Style::default().fg(FAILURE).add_modifier(Modifier::BOLD)
}

pub fn subtle_border() -> Style {
  Style::default().fg(BORDER).add_modifier(Modifier::DIM)
}

pub fn focused_border() -> Style {
  Style::default().fg(ACCENT)
}

pub fn selected() -> Style {
  Style::default().add_modifier(Modifier::REVERSED)
}

pub fn added() -> Style {
  success()
}

pub fn modified() -> Style {
  warning()
}

pub fn deleted() -> Style {
  failure()
}
