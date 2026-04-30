use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, Event};
use ratatui::Frame;

use crate::ui::TerminalOutputSuppression;

pub(super) enum FullscreenControl {
    Continue,
    Exit,
}

pub(super) trait FullscreenScreen {
    fn before_draw(&mut self) -> Result<()> {
        Ok(())
    }

    fn draw(&mut self, frame: &mut Frame<'_>);

    fn after_initial_draw(&mut self) -> Result<()> {
        Ok(())
    }

    fn should_exit(&mut self) -> bool;

    fn handle_event(&mut self, event: Event) -> Result<FullscreenControl>;

    fn tick(&mut self);
}

pub(super) fn with_fullscreen_terminal<T>(
    f: impl FnOnce(&mut ratatui::DefaultTerminal) -> Result<T>,
) -> Result<T> {
    let _terminal_output_suppression = TerminalOutputSuppression::enter();
    let mut terminal = ratatui::try_init().context("failed to initialize TUI terminal")?;
    let result = f(&mut terminal);
    let cleanup = ratatui::try_restore().context("failed to restore terminal after TUI session");

    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(err), Ok(())) | (Ok(_), Err(err)) => Err(err),
        (Err(err), Err(cleanup_err)) => Err(err.context(format!(
            "fullscreen terminal cleanup failed: {cleanup_err:#}"
        ))),
    }
}

pub(super) fn run_fullscreen_screen(
    screen: &mut impl FullscreenScreen,
    tick_rate: Duration,
) -> Result<()> {
    with_fullscreen_terminal(|terminal| {
        terminal.draw(|frame| screen.draw(frame))?;
        screen.after_initial_draw()?;

        let mut last_tick = Instant::now();
        loop {
            screen.before_draw()?;
            terminal.draw(|frame| screen.draw(frame))?;

            if screen.should_exit() {
                break;
            }

            let timeout = tick_rate.saturating_sub(last_tick.elapsed());
            if event::poll(timeout)? {
                let event = event::read()?;
                if matches!(screen.handle_event(event)?, FullscreenControl::Exit) {
                    break;
                }
            }

            if last_tick.elapsed() >= tick_rate {
                screen.tick();
                last_tick = Instant::now();
            }
        }

        Ok(())
    })
}
