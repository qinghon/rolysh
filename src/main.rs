#![feature(bstr)]
// Rolysh - Modern Rust architecture for parallel SSH connections

//
// This is the new, refactored main entry point using:
// - Real async I/O (no polling)
// - No global mutable state
// - Clean module boundaries
// - Type-safe state management

mod async_io;
mod cli;
mod config;
mod display_names;
mod errors;
mod host_syntax;
mod remote;
mod session;
mod ssh;
use crate::cli::{get_fd_limit, set_fd_limit};
use errors::Result;
use session::SessionManager;
use std::io::{self, IsTerminal};
use tracing::{debug, error, warn};

#[tokio::main]
async fn main() {
	// Run the main program
	let exit_code = run().await.unwrap_or_else(|e| {
		error!("Error: {e}");
		1
	});

	std::process::exit(exit_code);
}

async fn run() -> Result<i32> {
	// Parse command line arguments
	let mut config = cli::parse_args()?;

	// Handle stdin input if not interactive
	config.command = find_non_interactive_command(config.command)?;

	// Determine if we're in interactive mode
	config.interactive = config.command.is_none() && io::stdin().is_terminal() && io::stdout().is_terminal();

	// Expand hostname syntax
	let hosts: Vec<String> = config.host_names.iter().flat_map(|h| host_syntax::expand_syntax(h)).collect();

	if hosts.is_empty() {
		return Err(errors::Error::InvalidArgs("No hosts given".into()));
	}

	// Initialize tracing subscriber with environment-based configuration
	// Can be controlled via RUST_LOG environment variable
	// e.g., RUST_LOG=debug, RUST_LOG=rolysh=trace
	let log_level = if config.debug {
		tracing::Level::DEBUG
	} else {
		tracing::Level::INFO
	};
	let (non_blocking, _guard) = if config.debug {
		let log_file = config.log_file.clone().unwrap_or("/tmp/rolysh.log".into());
		let file_appender = tracing_appender::rolling::never(log_file.parent().unwrap(), log_file.file_name().unwrap());
		tracing_appender::non_blocking(file_appender)
	} else {
		tracing_appender::non_blocking(io::stdout())
	};

	tracing_subscriber::fmt()
		.with_max_level(log_level)
		.with_target(false)
		.with_thread_ids(false)
		.with_env_filter(
			tracing_subscriber::EnvFilter::try_from_default_env()
				.unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level.as_str())),
		)
		.with_writer(non_blocking)
		.init();
	// check ulimit setting
	if let Ok(mut lim) = get_fd_limit() {
		debug!("lim {:?}", lim);
		if lim.rlim_cur as usize <= hosts.len() * 2 && lim.rlim_max as usize >= hosts.len() * 2 + 32 {
			lim.rlim_cur = lim.rlim_max;
			match set_fd_limit(lim) {
				Ok(()) => {}
				Err(e) => {
					warn!("can't set fd limit: {e}");
				}
			}
		}
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

	Ok(if buffer.is_empty() { command } else { Some(buffer) })
}
