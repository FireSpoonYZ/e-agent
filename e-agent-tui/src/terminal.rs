use std::io;

use anyhow::Result;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};

use crate::render::ScreenMode;

pub trait TerminalDriver {
    fn start(&mut self, mode: ScreenMode) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
}

#[derive(Default)]
pub struct CrosstermDriver {
    raw: bool,
    alternate: bool,
    mouse: bool,
}
impl TerminalDriver for CrosstermDriver {
    fn start(&mut self, mode: ScreenMode) -> Result<()> {
        enable_raw_mode()?;
        self.raw = true;
        if mode == ScreenMode::Alternate {
            if let Err(error) = execute!(io::stdout(), EnterAlternateScreen) {
                let _ = self.stop();
                return Err(error.into());
            }
            self.alternate = true;
        }
        if let Err(error) = execute!(io::stdout(), EnableMouseCapture) {
            let _ = self.stop();
            return Err(error.into());
        }
        self.mouse = true;
        Ok(())
    }
    fn stop(&mut self) -> Result<()> {
        if self.mouse {
            let _ = execute!(io::stdout(), DisableMouseCapture);
            self.mouse = false;
        }
        if self.alternate {
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            self.alternate = false;
        }
        if self.raw {
            let _ = disable_raw_mode();
            self.raw = false;
        }
        Ok(())
    }
}
impl Drop for CrosstermDriver {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

pub struct TerminalSession<D: TerminalDriver> {
    driver: D,
}
impl<D: TerminalDriver> TerminalSession<D> {
    pub fn start(mut driver: D, mode: ScreenMode) -> Result<Self> {
        driver.start(mode)?;
        Ok(Self { driver })
    }
    pub fn stop(&mut self) -> Result<()> {
        self.driver.stop()
    }
}
impl<D: TerminalDriver> Drop for TerminalSession<D> {
    fn drop(&mut self) {
        let _ = self.driver.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};
    struct Fake {
        log: Rc<RefCell<Vec<&'static str>>>,
        fail: bool,
        started: bool,
    }
    impl TerminalDriver for Fake {
        fn start(&mut self, mode: ScreenMode) -> Result<()> {
            self.log.borrow_mut().push(match mode {
                ScreenMode::Main => "start-main",
                ScreenMode::Alternate => "start-alternate",
            });
            self.started = true;
            if self.fail {
                self.stop()?;
                anyhow::bail!("setup")
            }
            Ok(())
        }
        fn stop(&mut self) -> Result<()> {
            if self.started {
                self.log.borrow_mut().push("stop");
                self.started = false;
            }
            Ok(())
        }
    }
    #[test]
    fn failed_setup_rolls_back_and_screen_modes_are_distinct() {
        let log = Rc::new(RefCell::new(Vec::new()));
        assert!(
            TerminalSession::start(
                Fake {
                    log: log.clone(),
                    fail: true,
                    started: false,
                },
                ScreenMode::Alternate,
            )
            .is_err()
        );
        let mut session = TerminalSession::start(
            Fake {
                log: log.clone(),
                fail: false,
                started: false,
            },
            ScreenMode::Main,
        )
        .unwrap();
        session.stop().unwrap();
        session.stop().unwrap();
        assert_eq!(
            *log.borrow(),
            vec!["start-alternate", "stop", "start-main", "stop"]
        );
    }
}
