#![feature(random)]// Rolysh - Modern Rust architecture for parallel SSH connections

//
// This is the new, refactored main entry point using:
// - Real async I/O (no polling)
// - No global mutable state
// - Clean module boundaries
// - Type-safe state management

mod async_io;
mod callbacks;
mod cli;
mod config;
mod errors;
mod host_syntax;
mod remote;
mod session;
mod ssh;
mod terminal;

use errors::Result;
use session::SessionManager;
use std::io::{self, IsTerminal};
use terminal::Terminal;

#[tokio::main]
async fn main() {
    // Run the main program
    let exit_code = match run().await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("Error: {e}");
            1
        }
    };

    std::process::exit(exit_code);
}

async fn run() -> Result<i32> {
    // Parse command line arguments
    let mut config = cli::parse_args()?;

    // Handle stdin input if not interactive
    config.command = find_non_interactive_command(config.command)?;

    // Determine if we're in interactive mode
    config.interactive = config.command.is_none()
        && io::stdin().is_terminal()
        && io::stdout().is_terminal();

    // Create terminal manager (RAII - auto cleanup on drop)
    let _terminal = Terminal::new(config.interactive)?;

    // Expand hostname syntax
    let hosts: Vec<String> = config
        .host_names
        .iter()
        .flat_map(|h| host_syntax::expand_syntax(h))
        .collect();

    if hosts.is_empty() {
        return Err(errors::Error::InvalidArgs("No hosts given".into()));
    }

    // Enable debug logging if requested
    if config.debug {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .init();
    }

    // Create and run the session manager
    let mut session = SessionManager::new(config, hosts).await?;
    let exit_code = session.run().await?;

    Ok(exit_code)
}

/// Find non-interactive command from stdin if available
fn find_non_interactive_command(command: Option<String>) -> Result<Option<String>> {
    if io::stdin().is_terminal() {
        return Ok(command);
    }

    // Read from stdin if not a terminal
    let stdin = io::stdin();
    let mut buffer = String::new();
    let mut handle = stdin.lock();

    // Read all of stdin
    std::io::Read::read_to_string(&mut handle, &mut buffer)?;

    if !buffer.is_empty() && command.is_some() {
        return Err(errors::Error::InvalidArgs(
            "--command and reading from stdin are incompatible".into(),
        ));
    }

    if !buffer.is_empty() && !buffer.ends_with('\n') {
        buffer.push('\n');
    }

    Ok(if buffer.is_empty() {
        command
    } else {
        Some(buffer)
    })
}
