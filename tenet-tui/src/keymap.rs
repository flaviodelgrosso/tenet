use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::tui::{
  action::{Action, HistoryFilter, Screen},
  app::Application,
};

pub fn map_key(app: &Application, key: KeyEvent) -> Action {
  if app.ui().overlay.is_some() {
    return overlay_key(key);
  }

  if app.ui().go_prefix {
    return match key.code {
      KeyCode::Char('r') => Action::Go(Screen::Run),
      KeyCode::Char('q') => Action::Go(Screen::Requirements),
      KeyCode::Char('c') => Action::Go(Screen::Checks),
      KeyCode::Char('d') => Action::Go(Screen::Changes),
      KeyCode::Char('h') => Action::Go(Screen::History),
      KeyCode::Esc => Action::Cancel,
      _ => Action::None,
    };
  }

  if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
    return Action::Exit;
  }

  match key.code {
    KeyCode::Char('q') => Action::Exit,
    KeyCode::Char('r') if !app.running() => Action::Start,
    KeyCode::Char('?') => Action::OpenHelp,
    KeyCode::Char(':') => Action::OpenPalette,
    KeyCode::Char('/') => Action::OpenSearch,
    KeyCode::Char('g') => Action::PrefixGo,
    KeyCode::Char('c') if app.ui().screen == Screen::Run => Action::Context,
    KeyCode::Up | KeyCode::Char('k') => Action::Navigate(-1),
    KeyCode::Down | KeyCode::Char('j') => Action::Navigate(1),
    KeyCode::PageUp => Action::Page(-1),
    KeyCode::PageDown => Action::Page(1),
    KeyCode::Home => Action::First,
    KeyCode::End => Action::Last,
    KeyCode::Enter => Action::Inspect,
    KeyCode::Char('1') => Action::Go(Screen::Run),
    KeyCode::Char('2') => Action::Go(Screen::Requirements),
    KeyCode::Char('3') => Action::Go(Screen::Checks),
    KeyCode::Char('4') => Action::Go(Screen::Changes),
    KeyCode::Char('t') if app.ui().screen == Screen::History => {
      Action::SetHistoryFilter(HistoryFilter::Controller)
    }
    KeyCode::Char('a') if app.ui().screen == Screen::History => {
      Action::SetHistoryFilter(HistoryFilter::All)
    }
    KeyCode::Char('w') if app.ui().screen == Screen::History => {
      Action::SetHistoryFilter(HistoryFilter::Workers)
    }
    KeyCode::Char('v') if app.ui().screen == Screen::History => {
      Action::SetHistoryFilter(HistoryFilter::Checks)
    }
    KeyCode::Char('e') if app.ui().screen == Screen::History => {
      Action::SetHistoryFilter(HistoryFilter::Errors)
    }
    _ => Action::None,
  }
}

fn overlay_key(key: KeyEvent) -> Action {
  match key.code {
    KeyCode::Esc => Action::Cancel,
    KeyCode::Enter => Action::Confirm,
    KeyCode::Backspace => Action::Backspace,
    KeyCode::Up | KeyCode::Char('k') => Action::Navigate(-1),
    KeyCode::Down | KeyCode::Char('j') => Action::Navigate(1),
    KeyCode::PageUp => Action::Page(-1),
    KeyCode::PageDown => Action::Page(1),
    KeyCode::Home => Action::First,
    KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => Action::Stop,
    KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => Action::Type(ch),
    _ => Action::None,
  }
}
