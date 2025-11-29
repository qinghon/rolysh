use nix::sys::termios::{self, Termios};
use std::io::{self, IsTerminal};

/// Terminal manager that saves and restores terminal settings
/// Uses RAII pattern to ensure cleanup on drop
pub struct Terminal {
    original_termios: Option<Termios>,
}

impl Terminal {
    /// Create a new terminal manager
    /// If `interactive` is true and stdin is a terminal, saves current settings
    pub fn new(interactive: bool) -> io::Result<Self> {
        let mut terminal = Terminal {
            original_termios: None,
        };

        if interactive && io::stdin().is_terminal() {
            terminal.save_settings()?;
        }

        Ok(terminal)
    }

    /// Save current terminal settings
    fn save_settings(&mut self) -> io::Result<()> {
        self.original_termios = Some(termios::tcgetattr(io::stdin())?);
        Ok(())
    }

    /// Restore original terminal settings
    pub fn restore_settings(&self) {
        if let Some(ref termios) = self.original_termios {
            let _ = termios::tcsetattr(
                io::stdin(),
                termios::SetArg::TCSADRAIN,
                termios,
            );
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.restore_settings();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_terminal_creation() {
        // Should not fail even if not interactive
        let terminal = Terminal::new(false);
        assert!(terminal.is_ok());
    }
}
