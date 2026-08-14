use tenet_domain::model::{
  RepositoryChange, Requirement, RequirementAssessment, RequirementStatus, VerificationReport,
};

use crate::tui::action::{Action, Effect, HistoryFilter, Overlay, Screen};

pub use tenet_projection::{
  check_detail, failure_preview, phase_label, requirement_status, status_label, Activity,
  ActivityCategory, RunProjection, WorkerSession,
};

#[derive(Debug, Clone, Default)]
pub struct FeedState {
  pub selected: usize,
  pub scroll: u16,
  pub follow: bool,
  pub unseen: usize,
  pub query: String,
}

#[derive(Debug, Clone, Default)]
pub struct RequirementsState {
  pub selected: usize,
  pub scroll: u16,
  pub query: String,
}

#[derive(Debug, Clone, Default)]
pub struct ChecksState {
  pub selected: usize,
  pub scroll: u16,
  pub query: String,
}

#[derive(Debug, Clone, Default)]
pub struct ChangesState {
  pub selected: usize,
  pub scroll: u16,
  pub query: String,
}

#[derive(Debug, Clone)]
pub struct HistoryState {
  pub selected: usize,
  pub scroll: u16,
  pub follow: bool,
  pub unseen: usize,
  pub query: String,
  pub filter: HistoryFilter,
}

impl Default for HistoryState {
  fn default() -> Self {
    Self {
      selected: 0,
      scroll: 0,
      follow: true,
      unseen: 0,
      query: String::new(),
      filter: HistoryFilter::All,
    }
  }
}

#[derive(Debug, Clone)]
pub struct UiState {
  pub screen: Screen,
  pub overlay: Option<Overlay>,
  pub go_prefix: bool,
  pub run: FeedState,
  pub requirements: RequirementsState,
  pub checks: ChecksState,
  pub changes: ChangesState,
  pub history: HistoryState,
}

impl Default for UiState {
  fn default() -> Self {
    Self {
      screen: Screen::Run,
      overlay: None,
      go_prefix: false,
      run: FeedState {
        follow: true,
        ..FeedState::default()
      },
      requirements: RequirementsState::default(),
      checks: ChecksState::default(),
      changes: ChangesState::default(),
      history: HistoryState::default(),
    }
  }
}

pub struct Application {
  project_name: String,
  run: RunProjection,
  ui: UiState,
}

impl Application {
  pub fn new(
    project_name: String,
    state: tenet_domain::model::State,
    catalog: Vec<Requirement>,
  ) -> Self {
    Self {
      project_name,
      run: RunProjection::new(state, catalog),
      ui: UiState::default(),
    }
  }

  pub fn project_name(&self) -> &str {
    &self.project_name
  }

  pub fn state(&self) -> &tenet_domain::model::State {
    self.run.state()
  }

  pub fn catalog(&self) -> &[Requirement] {
    self.run.catalog()
  }

  pub fn assessments(&self) -> &std::collections::BTreeMap<String, RequirementAssessment> {
    self.run.assessments()
  }

  pub fn activities(&self) -> &std::collections::VecDeque<Activity> {
    self.run.activities()
  }

  pub fn current_worker(&self) -> Option<&WorkerSession> {
    self.run.current_worker()
  }

  pub fn checks(&self) -> &[VerificationReport] {
    self.run.checks()
  }

  pub fn changes(&self) -> &[RepositoryChange] {
    self.run.changes()
  }
  pub fn active_work_units(&self) -> impl Iterator<Item = &tenet_domain::model::WorkUnit> {
    self.run.active_work_units()
  }

  pub fn ui(&self) -> &UiState {
    &self.ui
  }

  pub fn running(&self) -> bool {
    self.run.running()
  }

  pub fn elapsed_seconds(&self) -> u64 {
    self.run.elapsed_seconds()
  }

  pub fn begin_run(&mut self) {
    self.run.begin_run();
    self.ui.run = FeedState {
      follow: true,
      ..FeedState::default()
    };
    self.ui.history = HistoryState::default();
    self.sync_follow();
  }

  pub fn apply(&mut self, event: tenet_domain::events::RunEvent) {
    let count = self.run.activities().len();
    self.run.apply(event);
    if self.run.activities().len() != count {
      self.sync_follow();
    }
  }

  fn sync_follow(&mut self) {
    if self.ui.run.follow {
      self.ui.run.selected = self.visible_feed().len().saturating_sub(1);
      self.ui.run.scroll = self
        .ui
        .run
        .selected
        .saturating_sub(5)
        .min(u16::MAX as usize) as u16;
    } else {
      self.ui.run.unseen = self.ui.run.unseen.saturating_add(1);
    }
    if self.ui.history.follow {
      self.ui.history.selected = self.visible_history().len().saturating_sub(1);
      self.ui.history.scroll = self
        .ui
        .history
        .selected
        .saturating_sub(5)
        .min(u16::MAX as usize) as u16;
    } else {
      self.ui.history.unseen = self.ui.history.unseen.saturating_add(1);
    }
  }

  pub fn dispatch(&mut self, action: Action) -> Effect {
    if let Some(overlay) = self.ui.overlay.clone() {
      return self.dispatch_overlay(overlay, action);
    }
    match action {
      Action::None => Effect::None,
      Action::Exit => {
        if self.running() {
          self.ui.overlay = Some(Overlay::ConfirmStop);
          Effect::None
        } else {
          Effect::Exit
        }
      }
      Action::Start => {
        if self.running() {
          Effect::None
        } else {
          Effect::Start
        }
      }
      Action::OpenHelp => {
        self.ui.overlay = Some(Overlay::Help);
        Effect::None
      }
      Action::OpenPalette => {
        self.ui.overlay = Some(Overlay::Palette {
          query: String::new(),
        });
        Effect::None
      }
      Action::OpenSearch => {
        self.ui.overlay = Some(Overlay::Search {
          query: self.current_query().to_owned(),
        });
        Effect::None
      }
      Action::Go(screen) => {
        self.ui.screen = screen;
        self.ui.go_prefix = false;
        Effect::None
      }
      Action::PrefixGo => {
        self.ui.go_prefix = true;
        Effect::None
      }
      Action::SetHistoryFilter(filter) => {
        self.ui.history.filter = filter;
        self.clamp_selection();
        Effect::None
      }
      Action::Navigate(amount) => {
        self.move_selection(amount);
        Effect::None
      }
      Action::Page(amount) => {
        self.move_selection(amount * 12);
        Effect::None
      }
      Action::First => {
        self.set_selected(0);
        self.unfollow();
        Effect::None
      }
      Action::Last => {
        self.follow_end();
        Effect::None
      }
      Action::Inspect => {
        self.open_inspector();
        Effect::None
      }
      Action::Context => {
        self.open_context();
        Effect::None
      }
      Action::Cancel => {
        self.ui.go_prefix = false;
        Effect::None
      }
      Action::Confirm | Action::Type(_) | Action::Backspace | Action::Stop => Effect::None,
    }
  }

  fn dispatch_overlay(&mut self, overlay: Overlay, action: Action) -> Effect {
    match overlay {
      Overlay::ConfirmStop => match action {
        Action::Confirm | Action::Stop => {
          self.ui.overlay = None;
          Effect::Stop
        }
        Action::Cancel | Action::Exit => {
          self.ui.overlay = None;
          Effect::None
        }
        _ => Effect::None,
      },
      Overlay::Inspector {
        title,
        body,
        mut scroll,
      } => match action {
        Action::Cancel => {
          self.ui.overlay = None;
          Effect::None
        }
        Action::Navigate(amount) | Action::Page(amount) => {
          scroll = scroll.saturating_add_signed(amount.saturating_mul(3) as i16);
          self.ui.overlay = Some(Overlay::Inspector {
            title,
            body,
            scroll,
          });
          Effect::None
        }
        Action::First => {
          self.ui.overlay = Some(Overlay::Inspector {
            title,
            body,
            scroll: 0,
          });
          Effect::None
        }
        _ => Effect::None,
      },
      Overlay::Help => {
        self.ui.overlay = None;
        Effect::None
      }
      Overlay::Search { mut query } => match action {
        Action::Cancel => {
          self.ui.overlay = None;
          Effect::None
        }
        Action::Confirm => {
          self.set_current_query(query);
          self.ui.overlay = None;
          self.clamp_selection();
          Effect::None
        }
        Action::Type(ch) => {
          query.push(ch);
          self.ui.overlay = Some(Overlay::Search { query });
          Effect::None
        }
        Action::Backspace => {
          query.pop();
          self.ui.overlay = Some(Overlay::Search { query });
          Effect::None
        }
        _ => Effect::None,
      },
      Overlay::Palette { mut query } => match action {
        Action::Cancel => {
          self.ui.overlay = None;
          Effect::None
        }
        Action::Confirm => {
          self.ui.overlay = None;
          self.execute_palette(&query)
        }
        Action::Type(ch) => {
          query.push(ch);
          self.ui.overlay = Some(Overlay::Palette { query });
          Effect::None
        }
        Action::Backspace => {
          query.pop();
          self.ui.overlay = Some(Overlay::Palette { query });
          Effect::None
        }
        _ => Effect::None,
      },
    }
  }

  fn execute_palette(&mut self, query: &str) -> Effect {
    let query = query.trim().to_ascii_lowercase();
    let action = match query.as_str() {
      "run" | "go run" => Action::Go(Screen::Run),
      "requirements" | "go requirements" => Action::Go(Screen::Requirements),
      "checks" | "go checks" => Action::Go(Screen::Checks),
      "changes" | "go changes" => Action::Go(Screen::Changes),
      "history" | "go history" => Action::Go(Screen::History),
      "start" | "resume" => Action::Start,
      "help" => Action::OpenHelp,
      _ => Action::None,
    };
    self.dispatch(action)
  }

  fn current_query(&self) -> &str {
    match self.ui.screen {
      Screen::Run => &self.ui.run.query,
      Screen::Requirements => &self.ui.requirements.query,
      Screen::Checks => &self.ui.checks.query,
      Screen::Changes => &self.ui.changes.query,
      Screen::History => &self.ui.history.query,
    }
  }

  fn set_current_query(&mut self, query: String) {
    match self.ui.screen {
      Screen::Run => self.ui.run.query = query,
      Screen::Requirements => self.ui.requirements.query = query,
      Screen::Checks => self.ui.checks.query = query,
      Screen::Changes => self.ui.changes.query = query,
      Screen::History => self.ui.history.query = query,
    }
  }

  pub fn visible_feed(&self) -> Vec<usize> {
    let query = self.ui.run.query.to_lowercase();
    self
      .activities()
      .iter()
      .enumerate()
      .filter(|(_, item)| !item.title.ends_with("TOOL COMPLETE"))
      .filter(|(_, item)| {
        query.is_empty()
          || format!("{} {} {}", item.title, item.summary, item.detail)
            .to_lowercase()
            .contains(&query)
      })
      .map(|(index, _)| index)
      .collect()
  }

  pub fn visible_requirements(&self) -> Vec<usize> {
    filtered_indexes(
      self
        .catalog()
        .iter()
        .map(|item| format!("{} {} {}", item.id, item.title, item.description)),
      &self.ui.requirements.query,
    )
  }

  pub fn visible_checks(&self) -> Vec<usize> {
    filtered_indexes(
      self.checks().iter().map(|item| {
        format!(
          "{} {}",
          if item.passed { "pass" } else { "fail" },
          failure_preview(item)
        )
      }),
      &self.ui.checks.query,
    )
  }

  pub fn visible_changes(&self) -> Vec<usize> {
    filtered_indexes(
      self
        .changes()
        .iter()
        .map(|item| format!("{} {}", item.status, item.path)),
      &self.ui.changes.query,
    )
  }

  pub fn visible_history(&self) -> Vec<usize> {
    self
      .activities()
      .iter()
      .enumerate()
      .filter(|(_, item)| history_match(item, self.ui.history.filter, &self.ui.history.query))
      .map(|(index, _)| index)
      .collect()
  }

  pub fn selected_requirement(&self) -> Option<&Requirement> {
    self
      .visible_requirements()
      .get(self.ui.requirements.selected)
      .and_then(|index| self.catalog().get(*index))
  }

  pub fn selected_check(&self) -> Option<&VerificationReport> {
    self
      .visible_checks()
      .get(self.ui.checks.selected)
      .and_then(|index| self.checks().get(*index))
  }

  pub fn selected_change(&self) -> Option<&RepositoryChange> {
    self
      .visible_changes()
      .get(self.ui.changes.selected)
      .and_then(|index| self.changes().get(*index))
  }

  pub fn selected_activity(&self) -> Option<&Activity> {
    let visible = match self.ui.screen {
      Screen::Run => self.visible_feed(),
      Screen::History => self.visible_history(),
      _ => return None,
    };
    visible
      .get(self.current_selected())
      .and_then(|index| self.activities().get(*index))
  }

  fn current_selected(&self) -> usize {
    match self.ui.screen {
      Screen::Run => self.ui.run.selected,
      Screen::Requirements => self.ui.requirements.selected,
      Screen::Checks => self.ui.checks.selected,
      Screen::Changes => self.ui.changes.selected,
      Screen::History => self.ui.history.selected,
    }
  }

  fn set_selected(&mut self, selected: usize) {
    let scroll = selected.saturating_sub(5).min(u16::MAX as usize) as u16;
    match self.ui.screen {
      Screen::Run => {
        self.ui.run.selected = selected;
        self.ui.run.scroll = scroll;
      }
      Screen::Requirements => {
        self.ui.requirements.selected = selected;
        self.ui.requirements.scroll = scroll;
      }
      Screen::Checks => {
        self.ui.checks.selected = selected;
        self.ui.checks.scroll = scroll;
      }
      Screen::Changes => {
        self.ui.changes.selected = selected;
        self.ui.changes.scroll = scroll;
      }
      Screen::History => {
        self.ui.history.selected = selected;
        self.ui.history.scroll = scroll;
      }
    }
  }

  fn visible_len(&self) -> usize {
    match self.ui.screen {
      Screen::Run => self.visible_feed().len(),
      Screen::Requirements => self.visible_requirements().len(),
      Screen::Checks => self.visible_checks().len(),
      Screen::Changes => self.visible_changes().len(),
      Screen::History => self.visible_history().len(),
    }
  }

  fn move_selection(&mut self, amount: i32) {
    let len = self.visible_len();
    let current = self.current_selected();
    self.set_selected(
      current
        .saturating_add_signed(amount as isize)
        .min(len.saturating_sub(1)),
    );
    self.unfollow();
  }

  fn clamp_selection(&mut self) {
    let len = self.visible_len();
    self.set_selected(self.current_selected().min(len.saturating_sub(1)));
  }

  fn unfollow(&mut self) {
    match self.ui.screen {
      Screen::Run => self.ui.run.follow = false,
      Screen::History => self.ui.history.follow = false,
      _ => {}
    }
  }

  fn follow_end(&mut self) {
    match self.ui.screen {
      Screen::Run => {
        self.ui.run.follow = true;
        self.ui.run.unseen = 0;
        self.ui.run.selected = self.visible_feed().len().saturating_sub(1);
        self.ui.run.scroll = self
          .ui
          .run
          .selected
          .saturating_sub(5)
          .min(u16::MAX as usize) as u16;
      }
      Screen::History => {
        self.ui.history.follow = true;
        self.ui.history.unseen = 0;
        self.ui.history.selected = self.visible_history().len().saturating_sub(1);
        self.ui.history.scroll = self
          .ui
          .history
          .selected
          .saturating_sub(5)
          .min(u16::MAX as usize) as u16;
      }
      _ => self.set_selected(self.visible_len().saturating_sub(1)),
    }
  }

  fn open_inspector(&mut self) {
    let item = match self.ui.screen {
      Screen::Requirements => self.selected_requirement().map(|item| {
        (
          format!("Requirement {}", item.id),
          requirement_detail(
            item,
            self.assessments().get(&item.id),
            self
              .active_work_units()
              .find(|work| work.requirement_ids.contains(&item.id)),
          ),
        )
      }),
      Screen::Checks => self
        .selected_check()
        .map(|item| ("Check evidence".into(), check_detail(item))),
      Screen::Changes => self.selected_change().map(|item| {
        (
          format!("Change {}", item.path),
          format!(
            "Status: {}\nPath: {}\n\nDiff detail is not emitted by the runtime yet.",
            item.status, item.path
          ),
        )
      }),
      Screen::Run | Screen::History => self
        .selected_activity()
        .map(|item| (item.title.clone(), activity_detail(item))),
    };

    if let Some((title, body)) = item {
      self.ui.overlay = Some(Overlay::Inspector {
        title,
        body,
        scroll: 0,
      });
    }
  }

  fn open_context(&mut self) {
    self.ui.overlay = Some(Overlay::Inspector {
      title: "Run context".into(),
      body: context_detail(self),
      scroll: 0,
    });
  }
}

fn filtered_indexes(values: impl Iterator<Item = String>, query: &str) -> Vec<usize> {
  let query = query.to_ascii_lowercase();
  values
    .enumerate()
    .filter_map(|(index, value)| {
      (query.is_empty() || value.to_ascii_lowercase().contains(&query)).then_some(index)
    })
    .collect()
}

fn history_match(item: &Activity, filter: HistoryFilter, query: &str) -> bool {
  let category = match filter {
    HistoryFilter::All => true,
    HistoryFilter::Controller => item.category == ActivityCategory::Controller,
    HistoryFilter::Workers => item.category == ActivityCategory::Worker,
    HistoryFilter::Checks => item.category == ActivityCategory::Check,
    HistoryFilter::Errors => item.category == ActivityCategory::Error,
  };

  category
    && (query.is_empty()
      || format!("{} {}", item.title, item.summary)
        .to_ascii_lowercase()
        .contains(&query.to_ascii_lowercase()))
}

fn requirement_detail(
  requirement: &Requirement,
  assessment: Option<&RequirementAssessment>,
  current: Option<&tenet_domain::model::WorkUnit>,
) -> String {
  let status = requirement_status(assessment);
  format!("{} · {}\nStatus: {}\n\n{}\n\nAcceptance criteria\n{}\n\nEvidence\n{}\n\nGaps\n{}\n\nCurrent work relationship\n{}", requirement.id, requirement.title, match status { RequirementStatus::Satisfied => "SATISFIED", RequirementStatus::Partial => "PARTIAL", RequirementStatus::Missing => "MISSING / UNASSESSED" }, requirement.description, bullets(&requirement.acceptance_criteria), assessment.map_or_else(|| "- No evidence assessed yet".into(), |item| bullets(&item.evidence)), assessment.map_or_else(|| "- Not assessed".into(), |item| bullets(&item.gaps)), current.filter(|work| work.requirement_ids.contains(&requirement.id)).map_or_else(|| "Not linked to the current work unit".into(), |work| format!("Linked to {} · {}", work.id, work.title)))
}

fn activity_detail(item: &Activity) -> String {
  format!(
    "{} · {}\n{}\n\n{}",
    item.category.label(),
    item.at,
    item.summary,
    item.detail
  )
}

fn work_detail(work: &tenet_domain::model::WorkUnit) -> String {
  format!(
    "{} · {}\n\nObjective\n{}\n\nRequirements\n{}",
    work.id,
    work.title,
    work.objective,
    bullets(&work.requirement_ids)
  )
}

fn context_detail(app: &Application) -> String {
  let state = app.state();
  let work = {
    let active = app.active_work_units().map(work_detail).collect::<Vec<_>>();
    if active.is_empty() {
      "No active work units".into()
    } else {
      active.join("\n\n")
    }
  };
  let checks = app.checks().last().map_or_else(
    || "No deterministic check recorded".into(),
    |check| {
      if check.passed {
        "PASS".into()
      } else {
        format!("FAIL · {}", failure_preview(check))
      }
    },
  );
  format!("Active work\n{work}\n\nRequirement health\n{}/{} satisfied · {} partial · {} missing\n\nCycle\n{}\n\nLatest verification\n{checks}\n\nRepository changes\n{} changed paths", state.requirement_counts.satisfied, state.requirement_counts.total, state.requirement_counts.partial, state.requirement_counts.missing, state.cycle, app.changes().len())
}

fn bullets(items: &[String]) -> String {
  if items.is_empty() {
    "- None".into()
  } else {
    items
      .iter()
      .map(|item| format!("- {item}"))
      .collect::<Vec<_>>()
      .join("\n")
  }
}
