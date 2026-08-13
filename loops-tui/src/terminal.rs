use std::io;

use anyhow::Result;
use crossterm::{
  execute,
  terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Frame, Terminal};

pub struct TerminalGuard {
  terminal: Terminal<CrosstermBackend<io::Stdout>>,
  restored: bool,
}

impl TerminalGuard {
  pub fn enter() -> Result<Self> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    if let Err(error) = execute!(stdout, EnterAlternateScreen) {
      let _ = disable_raw_mode();
      return Err(error.into());
    }
    let backend = CrosstermBackend::new(stdout);
    match Terminal::new(backend) {
      Ok(mut terminal) => {
        terminal.clear()?;
        Ok(Self {
          terminal,
          restored: false,
        })
      }
      Err(error) => {
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        let _ = disable_raw_mode();
        Err(error.into())
      }
    }
  }

  pub fn draw(&mut self, render: impl FnOnce(&mut Frame<'_>)) -> Result<()> {
    self.terminal.draw(render)?;
    Ok(())
  }

  pub fn restore(&mut self) -> Result<()> {
    if self.restored {
      return Ok(());
    }
    self.terminal.show_cursor()?;
    execute!(self.terminal.backend_mut(), LeaveAlternateScreen)?;
    disable_raw_mode()?;
    self.restored = true;
    Ok(())
  }
}

impl Drop for TerminalGuard {
  fn drop(&mut self) {
    let _ = self.restore();
  }
}
