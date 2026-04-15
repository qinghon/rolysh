use crate::config::Config;
use crate::errors::{Error, Result};
use crate::ssh::ShellType;
use std::env;
use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use std::str::FromStr;

pub fn parse_args() -> Result<Config> {
	let args: Vec<String> = env::args().collect();
	let mut config = Config::default();
	let mut i = 1;

	while i < args.len() {
		match args[i].as_str() {
			"--version" => {
				println!("rolysh 0.1.0");
				std::process::exit(0);
			}
			"-h" | "--help" => {
				print_help();
				std::process::exit(0);
			}
			"--hosts-file" => {
				i += 1;
				if i >= args.len() {
					return Err(Error::InvalidArgs("--hosts-file requires an argument".into()));
				}
				let hosts = Config::load_hosts_file(&args[i])
					.map_err(|e| Error::InvalidArgs(format!("Failed to read hosts file: {e}")))?;
				config.host_names.extend(hosts);
			}
			"--command" => {
				i += 1;
				if i >= args.len() {
					return Err(Error::InvalidArgs("--command requires an argument".into()));
				}
				config.command = Some(args[i].clone());
			}
			"--ssh" => {
				i += 1;
				if i >= args.len() {
					return Err(Error::InvalidArgs("--ssh requires an argument".into()));
				}
				config.ssh_cmd = args[i].clone();
			}
			"--user" => {
				i += 1;
				if i >= args.len() {
					return Err(Error::InvalidArgs("--user requires an argument".into()));
				}
				config.user = Some(args[i].clone());
			}
			"--no-color" => {
				config.disable_color = true;
			}
			"--password-file" => {
				i += 1;
				if i >= args.len() {
					return Err(Error::InvalidArgs("--password-file requires an argument".into()));
				}
				let password_file = &args[i];
				config.password = Some(read_password(password_file)?);
			}
			"--password" => {
				i += 1;
				if i >= args.len() {
					return Err(Error::InvalidArgs("--password requires an argument".into()));
				}
				let password = &args[i];
				config.password = Some(password.clone());
			}
			"--log-file" => {
				i += 1;
				if i >= args.len() {
					return Err(Error::InvalidArgs("--log-file requires an argument".into()));
				}
				config.log_file = Some(PathBuf::from(&args[i]));
			}
			"--abort-errors" => {
				config.abort_on_error = true;
			}
			"--debug" => {
				config.debug = true;
			}
			"--force-shell" => {
				i += 1;
				if i >= args.len() {
					return Err(Error::InvalidArgs("--force-shell requires an argument".into()));
				}
				config.force_shell = ShellType::from_str(&args[i])?;
			}
			arg if arg.starts_with("--") => {
				return Err(Error::InvalidArgs(format!("Unknown option: {arg}")));
			}
			_ => {
				// This is a hostname
				config.host_names.push(args[i].clone());
			}
		}
		i += 1;
	}

	// Handle reading from stdin if no command was provided
	let mut stdin = io::stdin();
	if config.command.is_none() && !stdin.is_terminal() {
		let mut stdin_input = String::new();
		stdin.read_to_string(&mut stdin_input)?;
		if !stdin_input.is_empty() {
			config.command = Some(stdin_input);
		}
	}

	// Determine if interactive mode
	config.interactive = config.command.is_none() && stdin.is_terminal() && io::stdout().is_terminal();

	if config.host_names.is_empty() {
		print_help();
		return Err(Error::InvalidArgs("No hosts specified".into()));
	}

	Ok(config)
}

fn read_password(file_path: &str) -> Result<String> {
	if file_path == "-" {
		// Read from terminal
		use std::io::Write;
		print!("Password: ");
		io::stdout().flush().ok();

		let mut password = String::new();
		io::stdin().read_line(&mut password)?;
		Ok(password.trim_end().to_string())
	} else {
		// Read from file
		std::fs::read_to_string(file_path).map(|s| s.trim_end().to_string()).map_err(Error::Io)
	}
}

pub(crate) fn print_help() {
	let def_conf = Config::default();
	println!(
		r#"rolysh - Control many SSH sessions at once

USAGE:
    rolysh [OPTIONS] HOSTS...

OPTIONS:
    --version              Show version
    -h, --help            Show this help message
    --hosts-file FILE     Read hostnames from a file
    --command CMD         Execute command and exit (non-interactive)
    --ssh SSH             SSH command to use default: {}
    --user USER           Remote user to log in as
    --no-color            Disable colored hostnames
    --password-file FILE  Read password from file (- for stdin)
    --password 'PASSWD'   set password from cli
    --log-file FILE       Log all I/O to a file
    --abort-errors        Exit on connection errors
    --debug               Enable debug output
    --force-shell         Set remote shell type, support bash,zsh,fish,auto

CONTROL COMMANDS (prefixed with ':'):
    :add NAMES...         Add remote shells
    :list [SHELLS...]     List shells and their state
    :disable [SHELLS...]  Disable shells
    :enable [SHELLS...]   Enable shells
    :quit                 Exit rolysh

"#,
		def_conf.ssh_cmd,
	);
}

pub(crate) fn get_fd_limit() -> Result<libc::rlimit> {
	let mut limit: libc::rlimit = unsafe { std::mem::zeroed() };
	if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0 {
		Ok(limit)
	} else {
		Err(io::Error::last_os_error().into())
	}
}
pub(crate) fn set_fd_limit(limit: libc::rlimit) -> Result<()> {
	if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) } != 0 {
		return Err(io::Error::last_os_error().into());
	}
	Ok(())
}
