pub use tenet_projection::{role_label, ActivityCategory};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
  Run,
  Requirements,
  Checks,
  Changes,
  History,
}

impl Screen {
  pub fn label(self) -> &'static str {
    match self {
      Self::Run => "Run",
      Self::Requirements => "Requirements",
      Self::Checks => "Checks",
      Self::Changes => "Changes",
      Self::History => "History",
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryFilter {
  All,
  Controller,
  Workers,
  Checks,
  Errors,
}

impl HistoryFilter {
  pub fn label(self) -> &'static str {
    match self {
      Self::All => "All",
      Self::Controller => "Controller",
      Self::Workers => "Workers",
      Self::Checks => "Checks",
      Self::Errors => "Errors",
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
  Help,
  Palette {
    query: String,
  },
  Search {
    query: String,
  },
  Inspector {
    title: String,
    body: String,
    scroll: u16,
  },
  ConfirmStop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
  None,
  Exit,
  Start,
  Stop,
  Cancel,
  Confirm,
  OpenHelp,
  OpenPalette,
  OpenSearch,
  Type(char),
  Backspace,
  Navigate(i32),
  Page(i32),
  First,
  Last,
  Inspect,
  Go(Screen),
  SetHistoryFilter(HistoryFilter),
  Context,
  PrefixGo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
  None,
  Start,
  Stop,
  Exit,
}
