use crate::ssh::ShellType;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
	pub host_names: Vec<String>,
	pub command: Option<String>,
	pub ssh_cmd: String,
	pub user: Option<String>,
	pub disable_color: bool,
	pub password: Option<String>,
	pub log_file: Option<PathBuf>,
	pub abort_on_error: bool,
	pub debug: bool,
	pub interactive: bool,
	pub force_shell: ShellType,
}

impl Default for Config {
	fn default() -> Self {
		Config {
			host_names: Vec::new(),
			command: None,
			ssh_cmd: "exec ssh -oLogLevel=Quiet -t %(host)s %(port)s".to_string(),
			user: None,
			disable_color: false,
			password: None,
			log_file: None,
			abort_on_error: false,
			debug: false,
			interactive: false,
			force_shell: Default::default(),
			// port_override: None,
		}
	}
}

impl Config {
	pub fn load_hosts_file(filename: &str) -> std::io::Result<Vec<String>> {
		let file = File::open(filename)?;
		let reader = BufReader::new(file);
		let hosts = reader
			.lines()
			.filter_map(|line| {
				if let Ok(line) = line {
					let line = line.trim();
					// Skip comments and empty lines
					if !line.is_empty() && !line.starts_with('#') {
						// Remove inline comments
						let line = line.split('#').next().unwrap_or("").trim();
						if !line.is_empty() {
							return Some(line.to_string());
						}
					}
				}
				None
			})
			.collect();
		Ok(hosts)
	}
}
