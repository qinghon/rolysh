#![allow(unused_attributes)]

use crate::config::Config;
use crate::display_names;
use crate::errors::Result;
use crate::remote::{Remote, RemoteCommand, RemoteConfig, RemoteEvent, RemoteState};
use reedline::{FileBackedHistory, Prompt, PromptEditMode, PromptHistorySearch, Reedline, Signal};
use std::borrow::Cow;
use std::bstr::ByteStr;
use std::collections::HashMap;
use std::env;
use std::fmt::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use tokio::select;
use tokio::sync::mpsc;
use tracing::{debug, error};

fn get_decimal_width(n: usize) -> usize {
	if n == 0 {
		return 1;
	}
	let mut width = 0;
	let mut temp = n;
	while temp > 0 {
		width += 1;
		temp /= 10;
	}
	width
}

#[derive(Debug, Default)]
struct InputPrompt {
	total: AtomicU32,
	ready: AtomicU32,
}
impl Prompt for InputPrompt {
	fn render_prompt_left(&self) -> Cow<'_, str> {
		let total = self.total.load(Ordering::Relaxed);
		let ready = self.ready.load(Ordering::Relaxed);
		let width = get_decimal_width(total as _);
		if total != ready {
			format!("wait ({:>width$}/{:<width$})> ", total - ready, total, width = width).into()
		} else {
			format!("ready ({:>width$})> ", total, width = width * 2).into()
		}
	}

	fn render_prompt_right(&self) -> Cow<'_, str> {
		Cow::from("")
	}

	fn render_prompt_indicator(&self, _prompt_mode: PromptEditMode) -> Cow<'_, str> {
		Cow::from("")
	}

	fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
		Cow::from("")
	}

	fn render_prompt_history_search_indicator(&self, _history_search: PromptHistorySearch) -> Cow<'_, str> {
		Cow::from("")
	}
}
impl InputPrompt {
	fn new(total: usize) -> Self {
		Self { total: AtomicU32::new(total as _), ready: AtomicU32::new(0) }
	}
	fn set_ready(&self, ready: u32) {
		self.ready.store(ready, Ordering::Relaxed);
	}
}

/// Session manager coordinates multiple remote connections
pub struct SessionManager {
	config: Config,
	/// Map from display_name to [RemoteHandle]
	// remotes: HashMap<String, RemoteHandle>,
	cmd_tx: tokio::sync::broadcast::Sender<RemoteCommand>,
	// event_rx: mpsc::Receiver<RemoteEvent>,
	// event_tx: mpsc::Sender<RemoteEvent>,
	/// Map from display_name to RemoteState
	remote_states: HashMap<usize, RemoteState>,
	exit_code: i32,
	// prompt_tx: Option<mpsc::Sender<Box<str>>>,
	hosts: Vec<Arc<str>>,
	display_names: Vec<Arc<str>>,
	max_name_length: usize,
	// len_width: usize,
}

impl SessionManager {
	/// Create a new session manager
	pub async fn new(config: Config, hosts: Vec<String>) -> Result<Self> {
		let remote_states = HashMap::from_iter(hosts.iter().enumerate().map(|(idx, _)| (idx, RemoteState::NotStarted)));

		let (display_names, max_name_length) = display_names::make_display_names(&hosts);
		let hosts: Vec<_> = hosts.into_iter().map(|x| Arc::from(x.as_str())).collect();
		let display_names: Vec<_> = display_names.into_iter().map(|x| Arc::from(x.as_str())).collect();
		// let len_width = get_decimal_width(hosts.len());
		let (cmd_tx, _) = tokio::sync::broadcast::channel(100);

		Ok(Self {
			config,
			cmd_tx,
			// event_rx,
			remote_states,
			exit_code: 0,
			hosts,
			display_names,
			max_name_length,
			// len_width,
		})
	}
	pub async fn start_remote(&self) -> Result<mpsc::Receiver<RemoteEvent>> {
		let config = &self.config;
		let (event_tx, event_rx) = mpsc::channel(1000);
		for (id, host) in self.hosts.iter().enumerate() {
			let (hostname, port) = parse_host_port(host);

			let remote_config = RemoteConfig {
				hostname,
				port,
				user: config.user.clone(),
				ssh_cmd: config.ssh_cmd.clone(),
				password: config.password.clone(),
				command: config.command.clone(),
				interactive: config.interactive,
				disable_color: config.disable_color,
				shell_type: config.force_shell,
			};

			let display_name = self.display_names[id].clone();
			let remote = Remote::new(id, remote_config, display_name);
			let cmd_tx_sub = self.cmd_tx.subscribe();
			let event_tx_clone = event_tx.clone();

			// Spawn remote task - use display_name as identifier in events
			tokio::spawn(async move { remote.start_loop(cmd_tx_sub, event_tx_clone).await });
		}
		Ok(event_rx)
	}

	/// Run the session in appropriate mode
	pub async fn run(&mut self) -> Result<i32> {
		if self.config.interactive {
			self.run_interactive().await?;
		} else {
			self.run_batch().await?;
		}
		Ok(self.exit_code)
	}

	/// Run in interactive mode
	async fn run_interactive(&mut self) -> Result<()> {
		let (stdin_tx, mut stdin_rx) = mpsc::channel::<Option<String>>(1);
		let waiting_input = Arc::new(AtomicBool::new(true));
		let input_send_wait = waiting_input.clone();

		let prompt = Arc::new(InputPrompt::new(self.remote_states.len() as _));
		let prompt_send = prompt.clone();
		let ext_printer = reedline::ExternalPrinter::<String>::new(128);
		let input_printer = ext_printer.clone();
		let event_printer = ext_printer.clone();

		let mut event_rx = self.start_remote().await?;

		// Spawn persistent stdin thread with editor instance
		let _stdin_handle = std::thread::Builder::new().name("input".into()).spawn(move || {
			let completer = Box::new(reedline::DefaultCompleter::new(vec![]));

			let mut line_editor = Reedline::create().with_external_printer(input_printer).with_completer(completer);
			let history = if let Ok(home) = env::var("HOME") {
				FileBackedHistory::with_file(1000, PathBuf::from(home).join(".rolysh_history"))
					.expect("cannot set history file")
			} else {
				FileBackedHistory::new(1000).unwrap()
			};

			line_editor = line_editor.with_history(Box::new(history));

			let prompt_tx = prompt_send.as_ref();

			loop {
				let sig = line_editor.read_line(prompt_tx);
				{
					match sig {
						Ok(s) => match s {
							Signal::Success(line) => {
								stdin_tx.blocking_send(Some(line)).unwrap();
							}
							Signal::CtrlC => {
								stdin_tx.blocking_send(Some("\x03".to_string())).unwrap();
							}
							Signal::CtrlD => {
								stdin_tx.blocking_send(Some("\x04".to_string())).unwrap();
							}
						},
						Err(e) => {
							error!("Error reading line: {}", e);
						}
					}
				}
				if !input_send_wait.load(Ordering::Relaxed) {
					//exit input waiting
					break;
				}
			}
		});

		let prompt_rx = prompt.clone();

		let (event_fwd_tx, mut event_fwd_rx) = mpsc::channel(self.hosts.len() * 2);

		let max_name_length = self.max_name_length;

		let _output_task = tokio::spawn(async move {
			let mut events_cache = Vec::with_capacity(16);

			while let event_num = event_rx.recv_many(&mut events_cache, 16).await
				&& event_num != 0
			{
				for event in events_cache.drain(..) {
					match event {
						RemoteEvent::Output { display_name: hostid, data, color } => {
							print_remote_output(&hostid, max_name_length, &data, color, Some(&ext_printer))
						}
						e => {
							let _ = event_fwd_tx.send(e).await;
						}
					}
				}
			}
		});

		loop {
			select! {

				// Handle stdin
				Some(line_opt) = stdin_rx.recv() => {
					// Handle the line if it exists
					if let Some(mut line) = line_opt {
						if line.is_empty() {
							continue;
						}
						if matches!(line.as_bytes()[0], b'\x03'| b'\x04') {
							let _ = self.cmd_tx.send(RemoteCommand::Send(line.into_bytes()));
							continue;
						}

						if let Some(cmd) = line.strip_prefix(':') {
							self.handle_control_command(cmd, &event_printer).await?;
						} else if let Some(cmd) = line.strip_prefix('!') {
							let _ = tokio::process::Command::new("sh")
								.arg("-c")
								.arg(cmd)
								.status()
								.await;
						} else {
							line.push('\n');
							self.send_to_all_enabled(line).await?;
						}
					} else {
						// Interrupted

					}
				},

				// Handle remote events
				Some(event) = event_fwd_rx.recv() => {
					self.handle_event(event, Some(&event_printer)).await;

					prompt_rx.set_ready(self.ready_num() as _);
					while let Ok(event) = event_fwd_rx.try_recv() {
						self.handle_event(event, Some(&event_printer)).await;
					}
				},

				else => break,
			}

			// Check if all remotes are terminated
			if self.all_terminated() {
				break;
			}
		}
		waiting_input.store(false, Ordering::SeqCst);
		let _ = _stdin_handle?.join();
		Ok(())
	}

	/// Run in batch mode
	async fn run_batch(&mut self) -> Result<()> {
		// Wait for all remotes to complete
		let mut event_rx = self.start_remote().await?;
		while let Some(event) = event_rx.recv().await {
			self.handle_event(event, None).await;

			if self.all_terminated() {
				break;
			}
		}

		Ok(())
	}

	/// Handle a remote event
	async fn handle_event(&mut self, event: RemoteEvent, printer: Option<&reedline::ExternalPrinter<String>>) {
		match event {
			RemoteEvent::Connected { hostid } => {
				debug!("[{hostid}] Connected");
			}
			RemoteEvent::StateChanged { hostid, state } => {
				self.remote_states.insert(hostid, state);
				debug!("[{}] State: {}", hostid, state);
			}
			RemoteEvent::Output { display_name: hostid, data, color } => {
				print_remote_output(&hostid, self.max_name_length, &data, color, printer);
			}
			RemoteEvent::Closed { hostid, exit_code } => {
				self.remote_states.insert(hostid, RemoteState::Terminated);
				if exit_code != 0 {
					self.exit_code = self.exit_code.max(exit_code);
					if self.config.interactive {
						print_remote_output(
							&self.display_names[hostid],
							self.max_name_length,
							format!("Exited with code {exit_code}").as_bytes(),
							0,
							printer,
						);
					}
				}
			}
			RemoteEvent::Error { hostid, error } => {
				error!("[{hostid}] Error: {error}");
			}
		}
	}

	/// Send command to all enabled remotes
	async fn send_to_all_enabled(&mut self, command: String) -> Result<()> {
		debug!("send command to all: {:?}", command);
		match self.cmd_tx.send(RemoteCommand::Send(command.into_bytes())) {
			Ok(_) => {}
			Err(e) => {
				error!("send command to all failed: {}", e);
			}
		}
		Ok(())
	}

	/// Handle control commands (:list, :quit, etc.)
	async fn handle_control_command(&mut self, cmd: &str, printer: &reedline::ExternalPrinter<String>) -> Result<()> {
		let parts: Vec<&str> = cmd.split_whitespace().collect();
		if parts.is_empty() {
			return Ok(());
		}

		match parts[0] {
			"list" | "l" => {
				let _ = printer.print("Remotes:".to_string());
				for hostid in 0..self.display_names.len() {
					let state = self.remote_states.get(&hostid).unwrap_or(&RemoteState::NotStarted);
					let _ = printer.print(format!("  {} - {}", self.display_names[hostid], state.as_str()));
				}
			}
			"quit" | "q" | "exit" => {
				// Close all remotes
				let _ = self.cmd_tx.send(RemoteCommand::Close(-1));
			}
			"enable" | "e" => {
				if parts.len() > 1 {
					for &display_name in &parts[1..] {
						if let Some(handle) = self.hosts.iter().position(|x| display_name.eq(x.as_str())) {
							let _ = self.cmd_tx.send(RemoteCommand::SetEnabled(handle as _, true));
							let _ = printer.print(format!("Enabled {display_name}"));
						}
					}
				} else {
					// Enable all
					let _ = self.cmd_tx.send(RemoteCommand::SetEnabled(-1, true));
					let _ = printer.print("Enabled all".to_string());
				}
			}
			"disable" | "d" => {
				if parts.len() > 1 {
					for &display_name in &parts[1..] {
						if let Some(hostid) = self.hosts.iter().position(|x| x.as_str() == display_name) {
							let _ = self.cmd_tx.send(RemoteCommand::SetEnabled(hostid as _, false));
							let _ = printer.print(format!("Disabled {display_name}"));
						}
					}
				} else {
					// Disable all
					let _ = self.cmd_tx.send(RemoteCommand::SetEnabled(-1, false));
					let _ = printer.print("Disabled all".to_string());
				}
			}
			"help" | "h" => {
				let _ = printer.print("Control commands:".to_string());
				let _ = printer.print("  :list, :l          - List all remotes and their states".to_string());
				let _ = printer.print("  :quit, :q, :exit   - Quit the session".to_string());
				let _ = printer.print("  :enable [hosts...] - Enable remotes (all if no args)".to_string());
				let _ = printer.print("  :disable [hosts...] - Disable remotes (all if no args)".to_string());
				let _ = printer.print("  :help, :h          - Show this help".to_string());
			}
			_ => {
				let _ = printer.print(format!("Unknown command: {}", parts[0]));
				let _ = printer.print("Type :help for available commands".to_string());
			}
		}

		Ok(())
	}

	fn ready_num(&self) -> usize {
		self.remote_states.values().filter(|&&s| s == RemoteState::Idle).count()
	}

	/// Check if all remotes are terminated
	fn all_terminated(&self) -> bool {
		self.remote_states.values().all(|&s| s == RemoteState::Terminated)
	}
}

/// Parse host:port format
fn parse_host_port(host: &str) -> (String, String) {
	let dem_num:usize = host.chars().map(|c| if c == ':' {1}else {0}).sum();
	if dem_num > 1 {
		if let Some(colon_idx) = host.rfind("]") {
			let (hostname, port_str) = host.split_at(colon_idx);
			(hostname[1..].to_string(), port_str[2..].to_string())
		}else {
			(host.to_string(), "22".to_string())
		}
	} else if let Some(colon_idx) = host.rfind(':') {
		let (hostname, port_str) = host.split_at(colon_idx);
		(hostname.to_string(), port_str[1..].to_string())
	} else {
		(host.to_string(), "22".to_string())
	}
}

/// Print remote output with display name prefix
fn print_remote_output(
	display_name: &str,
	prefix_len: usize,
	data: &[u8],
	color: u8,
	printer: Option<&reedline::ExternalPrinter<String>>,
) {
	let mut line = String::with_capacity(prefix_len + 32 + data.len());
	if color != 0 {
		let _ = line.write_fmt(format_args!(
			"\x1b[1;{color}m{:<width$}\x1b[1;m : \x1b[0m",
			display_name,
			width = prefix_len
		));
	} else {
		let _ = line.write_fmt(format_args!("{:<width$} : ", display_name, width = prefix_len));
	}

	let output = ByteStr::new(data);
	let _ = line.write_fmt(format_args!("{output}"));

	if let Some(printer) = printer {
		let _ = printer.print(line);
	} else {
		print!("\r\r{line}");
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_parse_host_port() {
		assert_eq!(parse_host_port("example.com"), ("example.com".to_string(), "22".to_string()));
		assert_eq!(parse_host_port("192.168.1.1"), ("192.168.1.1".to_string(), "22".to_string()));
		assert_eq!(parse_host_port("fe80::1"), ("fe80::1".to_string(), "22".to_string()));
		assert_eq!(parse_host_port("fe80::1%eth0"), ("fe80::1%eth0".to_string(), "22".to_string()));
		assert_eq!(parse_host_port("[fe80::1]:23"), ("fe80::1".to_string(), "23".to_string()));
		assert_eq!(parse_host_port("example.com:2222"), ("example.com".to_string(), "2222".to_string()));
	}
}
