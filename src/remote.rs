use crate::async_io::ReadBuf;
use crate::errors::Result;
use crate::ssh::{PtyWriteHalf, SHELL_DECATE_START, ShellType, SshProcess, fmt_shell_prompt};
use crate::ssh::{fmt_prompt, search_prompt};
use std::bstr::ByteStr;
use std::fmt::Display;
use std::io::IsTerminal;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::select;
use tokio::sync::{broadcast, mpsc};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, instrument};

/// Remote connection state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RemoteState {
	NotStarted,
	Connecting,
	Idle,
	Running,
	Terminated,
}

impl RemoteState {
	pub fn as_str(&self) -> &str {
		match self {
			RemoteState::NotStarted => "not_started",
			RemoteState::Connecting => "connecting",
			RemoteState::Idle => "idle",
			RemoteState::Running => "running",
			RemoteState::Terminated => "terminated",
		}
	}
}
impl From<u8> for RemoteState {
	fn from(state: u8) -> Self {
		match state {
			0 => RemoteState::NotStarted,
			1 => RemoteState::Connecting,
			2 => RemoteState::Idle,
			3 => RemoteState::Running,
			4 => RemoteState::Terminated,
			_ => RemoteState::NotStarted,
		}
	}
}
impl From<RemoteState> for u8 {
	fn from(val: RemoteState) -> Self {
		match val {
			RemoteState::NotStarted => 0u8,
			RemoteState::Connecting => 1u8,
			RemoteState::Idle => 2u8,
			RemoteState::Running => 3u8,
			RemoteState::Terminated => 4u8,
		}
	}
}

impl From<RemoteState> for AtomicU8 {
	fn from(val: RemoteState) -> Self {
		AtomicU8::new(val.into())
	}
}
impl Display for RemoteState {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}", self.as_str())
	}
}
#[derive(Debug)]
struct State {
	pub id: usize,
	/// repr[RemoteState]
	pub state: AtomicU8,
	pub event_tx: mpsc::Sender<RemoteEvent>,
}
impl State {
	fn state(&self) -> RemoteState {
		self.state.load(Ordering::Relaxed).into()
	}
	async fn change_state(&self, new_state: RemoteState) {
		if new_state != self.state() {
			// self.print_debug(format!("state => {}", new_state.as_str()).as_bytes());
			self.state.store(new_state.into(), Ordering::Relaxed);

			let _ = self.event_tx.send(RemoteEvent::StateChanged { hostid: self.id, state: new_state }).await;
		}
	}
}

/// Commands that can be sent to a remote connection
#[derive(Debug, Clone)]
pub enum RemoteCommand {
	/// Send data to remote
	Send(Vec<u8>),
	/// Close the connection
	Close(isize),
	/// Enable/disable the connection
	SetEnabled(isize, bool),
	// Rename the display name
	// Rename(String),
}

/// Events emitted by remote connections
#[derive(Debug, Clone)]
pub enum RemoteEvent {
	/// Connection established
	Connected { hostid: usize },
	/// State changed
	StateChanged { hostid: usize, state: RemoteState },
	/// Output received
	Output { display_name: Arc<str>, data: Vec<u8>, color: u8 },
	/// Connection closed
	Closed { hostid: usize, exit_code: i32 },
	/// Error occurred
	Error { hostid: usize, error: String },
}

/// Remote connection configuration
pub struct RemoteConfig {
	pub hostname: String,
	pub port: String,
	pub user: Option<String>,
	pub ssh_cmd: String,
	pub password: Option<String>,
	pub command: Option<String>,
	pub interactive: bool,
	pub disable_color: bool,
	pub shell_type: ShellType,
}

/// Remote connection task
pub struct Remote {
	id: usize,
	config: RemoteConfig,
	display_name: Arc<str>,
	color_code: u8,
	ssh_process: Option<SshProcess>,
	// line_reader: LineReader,
	shell_type: ShellType,
	shell_detection_sent: bool,
	shell_detection_buffer: Vec<u8>,
	init_sent: bool,
	read_in_not_started: Vec<u8>,
	prompt_prefix: String,
}

impl Remote {
	/// Create a new remote connection
	pub fn new(id: usize, config: RemoteConfig, display_name: Arc<str>) -> Self {
		// Assign color if enabled
		let color_code = if !config.disable_color && std::io::stdout().is_terminal() {
			rotate_color(id)
		} else {
			0
		};

		let shell_type = config.shell_type;

		Self {
			id,
			config,
			display_name,
			color_code,
			ssh_process: None,
			// line_reader: LineReader::new(),
			shell_type,
			shell_detection_sent: false,
			shell_detection_buffer: Vec::new(),
			init_sent: false,
			read_in_not_started: Vec::new(),
			prompt_prefix: String::new(),
		}
	}

	/// Run the remote connection task
	#[instrument(skip_all, fields(id = self.id, name = self.display_name.as_ref()))]
	pub async fn start_loop(
		mut self,
		cmd_rx: broadcast::Receiver<RemoteCommand>,
		event_tx: mpsc::Sender<RemoteEvent>,
	) -> Result<i32> {
		// Establish SSH connection
		let (ssh_process, pty) = match SshProcess::spawn(
			&self.config.hostname,
			&self.config.port,
			self.config.user.as_deref(),
			&self.config.ssh_cmd,
		) {
			Ok(ssh) => ssh,
			Err(e) => {
				let _ = event_tx
					.send(RemoteEvent::Error { hostid: self.id, error: format!("Failed to spawn SSH: {e}") })
					.await;
				return Err(e);
			}
		};

		self.ssh_process = Some(ssh_process);
		let (mut pty_read, pty_write) = pty.split()?;

		let state = Arc::new(State { id: self.id, state: RemoteState::NotStarted.into(), event_tx: event_tx.clone() });
		// Emit connected event
		let _ = event_tx.send(RemoteEvent::Connected { hostid: self.id }).await;

		state.change_state(RemoteState::Connecting).await;

		let mut exit_code = 0;
		let (write_tx, write_rx) = mpsc::channel::<Vec<u8>>(2);
		let cancel = CancellationToken::new();

		let write_handle = tokio::spawn(Self::write_loop(
			self.id,
			self.display_name.clone(),
			state.clone(),
			cmd_rx,
			cancel.clone(),
			write_rx,
			pty_write,
		));
		let default_keepalive_time = Duration::from_secs(600);
		let mut no_read_timeout = tokio::time::sleep(default_keepalive_time);
		// let mut read_buf = vec![0u8; 4096];
		let mut str_buf = Vec::with_capacity(512);
		let mut read_buf = ReadBuf::new(4096);

		// Main event loop
		loop {
			let tmp_buf = read_buf.get_write_buf();
			select! {
				// Read from SSH
				result = pty_read.read(tmp_buf) => {
					match result {
						Ok(0) => {
							// EOF - connection closed
							debug!("[{}]EOF received", state.state());
							break;
						}
						Ok(n) => {
							debug!("↓ [{}] {:?}", state.state(), ByteStr::new(&tmp_buf[..n]));
							read_buf.write_len(n);
							// let l = n;
							let t = Instant::now();
							self.handle_read_line(&mut read_buf, &mut str_buf, &state, &event_tx, &write_tx).await;
							debug!("process data time {:?}", t.elapsed());

						}
						Err(e) => {
							debug!("Read error: {}", e);
							break;
						}
					}
					no_read_timeout = tokio::time::sleep(Duration::from_millis(100));
					continue;
				},
				_ = no_read_timeout => {
					debug!("timeout on no readed");
					// keepalive
					let _ = write_tx.send(vec![]).await;

				},

				else => break,
			}
			no_read_timeout = tokio::time::sleep(default_keepalive_time);
		}
		drop(write_tx);
		cancel.cancel();
		// Wait for process to exit
		if let Some(ssh) = &self.ssh_process
			&& let Ok(Some(code)) = ssh.try_wait()
		{
			exit_code = code;
		}
		let _ = write_handle.await;

		state.change_state(RemoteState::Terminated).await;

		// Emit closed event
		let _ = event_tx.send(RemoteEvent::Closed { hostid: self.id, exit_code }).await;

		Ok(exit_code)
	}
	#[instrument(skip_all, fields(id = self_id, name = self_name.as_ref()))]
	async fn write_loop(
		self_id: usize,
		self_name: Arc<str>,
		state: Arc<State>,
		mut cmd_rx: broadcast::Receiver<RemoteCommand>,
		cancel_token: CancellationToken,
		mut write_rx: mpsc::Receiver<Vec<u8>>,
		mut pty_write: PtyWriteHalf,
	) {
		let mut enabled = true;

		let mut write_fn = async |data: &[u8]| {
			let t = Instant::now();
			let _ = pty_write.write_all(data).await;
			debug!("↑ [{}]: {:?} {:?}", state.state(), t.elapsed(), ByteStr::new(data));
		};

		loop {
			select! {
				Ok(cmd) = cmd_rx.recv() => {
					match cmd {
					RemoteCommand::Send(data) => {
						if enabled {
							if state.state() == RemoteState::Idle {
								state.change_state(RemoteState::Running).await;
							}
							write_fn(&data).await;
						}
					}
					RemoteCommand::Close(hostid) => {
						if hostid as usize == self_id || hostid == -1 {
							break;
						}
					}
					RemoteCommand::SetEnabled(hostid, enable) => {
						if hostid as usize == self_id || hostid == -1 {
							enabled = enable;
						}
					}
					// RemoteCommand::Rename(name) => {
					// 	self.display_name = name;
					// }
					}
				}
				Some(data) = write_rx.recv() => {
					write_fn(&data).await;
				}
				_ = cancel_token.cancelled() => {
					break;
				}
				else => break,
			}
		}
	}
	async fn handle_read_line(
		&mut self,
		read_buf: &mut ReadBuf,
		str_buf: &mut Vec<u8>,
		state: &Arc<State>,
		event_tx: &mpsc::Sender<RemoteEvent>,
		write_tx: &mpsc::Sender<Vec<u8>>,
	) {
		loop {
			match read_buf.read_line(str_buf) {
				Ok(n) => {
					if n == 0 {
						break;
					}
					self.handle_env_decate(state, str_buf, &[], event_tx, write_tx).await;
					self.handle_read(str_buf, state, event_tx, write_tx).await;
					str_buf.clear();
				}
				Err(e) => {
					if e.kind() == std::io::ErrorKind::WouldBlock {
						self.handle_env_decate(state, &[], str_buf, event_tx, write_tx).await;
						// str_buf.clear();
					}
				}
			}
		}
	}
	async fn handle_env_decate(
		&mut self,
		state: &Arc<State>,
		data: &[u8],
		half_line: &[u8],
		event_tx: &mpsc::Sender<RemoteEvent>,
		write_tx: &mpsc::Sender<Vec<u8>>,
	) {
		if state.state() != RemoteState::Connecting {
			return;
		}
		let line = String::from_utf8_lossy(half_line);
		let line_str = line.to_lowercase();
		if line_str.contains("password:") {
			debug!("find password prompt {:?}", line_str);
			if let Some(pwd) = &self.config.password {
				let _ = write_tx.send(format!("{pwd}\n").into()).await;
			} else {
				let _ = event_tx
					.send(RemoteEvent::Output {
						display_name: self.display_name.clone(),
						data: half_line.to_vec(),
						color: self.color_code,
					})
					.await;
				return;
			}
		}

		// Check for SSH errors
		if line_str.contains("the authenticity of host") || line_str.contains("host identification has changed") {
			let _ = event_tx
				.send(RemoteEvent::Output {
					display_name: self.display_name.clone(),
					data: data.to_vec(),
					color: self.color_code,
				})
				.await;
			self.read_in_not_started.extend_from_slice(data);
			return;
		}
		// Step 1: Send shell detection command if not sent yet
		if !self.shell_detection_sent && self.shell_type == ShellType::Unknown {
			let detect_cmd = SshProcess::shell_detection_command();
			let _ = write_tx.send(detect_cmd.into()).await;
			self.shell_detection_sent = true;

			debug!("[{}] Shell detection command sent", self.display_name);
			return;
		}

		// Step 2: Collect data into detection buffer if we haven't determined shell type yet
		if self.shell_type == ShellType::Unknown {
			self.shell_detection_buffer.extend_from_slice(data);

			// Try to detect shell type from collected data
			if let Some(detected_type) = SshProcess::detect_shell_from_output(&self.shell_detection_buffer) {
				self.shell_type = detected_type;

				debug!("✓ Detected shell type: {:?}", self.shell_type);
			}
		}

		// Step 3: Send init commands once we know the shell type
		if !self.init_sent && self.shell_type != ShellType::Unknown {
			debug!("Sending init commands for {:?}", self.shell_type);

			let (prompt, prefix) = fmt_prompt(self.id);
			let mut init_cmds = fmt_shell_prompt(self.shell_type, &prompt);
			init_cmds.push(b'\n');
			// some shell (fish) insert other char before end of prompt "\n"
			self.prompt_prefix = prefix;

			let _ = write_tx.send(init_cmds).await;

			self.init_sent = true;

			debug!("Initialization complete");
		}
	}
	/// Handle incoming data
	async fn handle_read(
		&mut self,
		data: &[u8],
		state: &Arc<State>,
		event_tx: &mpsc::Sender<RemoteEvent>,
		write_tx: &mpsc::Sender<Vec<u8>>,
	) {
		let was_running = state.state() == RemoteState::Running;
		let mut prompt_found = false;

		'outer: {
			if let Some(remaining_after_prompt) = search_prompt(&self.prompt_prefix, data) {
				debug!(
					"Prompt detected and send (state={}): {:?}",
					state.state(),
					ByteStr::new(remaining_after_prompt)
				);
				let _ = event_tx
					.send(RemoteEvent::Output {
						display_name: self.display_name.clone(),
						data: remaining_after_prompt.to_vec(),
						color: self.color_code,
					})
					.await;

				prompt_found = true;

				if state.state() == RemoteState::Connecting {
					self.handle_prompt_seen(state, write_tx).await;
				}
				break 'outer;
			}
			match state.state() {
				RemoteState::Idle | RemoteState::Running => {
					debug!("send output (state={}): {:?}", state.state(), ByteStr::new(&data));
					match event_tx
						.send(RemoteEvent::Output {
							display_name: self.display_name.clone(),
							data: data.to_vec(),
							color: self.color_code,
						})
						.await
					{
						Ok(_) => {}
						Err(e) => {
							error!("[{}] Failed to send output: {:?}", self.display_name, e);
						}
					}
				}
				RemoteState::Connecting => {
					let skip = data
						.windows(SHELL_DECATE_START.len())
						.position(|w| w == SHELL_DECATE_START.as_bytes())
						.is_some();

					if !skip {
						debug!("send output (state={}): {:?}", state.state(), ByteStr::new(data));
						let _ = event_tx
							.send(RemoteEvent::Output {
								display_name: self.display_name.clone(),
								data: data.to_vec(),
								color: self.color_code,
							})
							.await;
					}
				}
				_ => {}
			}
		}

		if prompt_found && was_running {
			debug!("PTY drained, changing state to Idle");
			state.change_state(RemoteState::Idle).await;
		}
	}

	/// Handle prompt seen for the first time
	async fn handle_prompt_seen(&mut self, state: &Arc<State>, write_tx: &mpsc::Sender<Vec<u8>>) {
		self.exec_pre_def_command(state, write_tx).await;
		state.change_state(RemoteState::Idle).await;
	}
	async fn exec_pre_def_command(&mut self, state: &Arc<State>, write_tx: &mpsc::Sender<Vec<u8>>) {
		if (!self.config.interactive)
			&& let Some(ref cmd) = self.config.command.clone()
		{
			// In non-interactive mode, send command and exit
			state.change_state(RemoteState::Running).await;

			let _ = write_tx.send(format!("{cmd}\n").into()).await;

			// Exit command - use appropriate syntax for shell type
			let exit_cmd: &[u8] = match self.shell_type {
				ShellType::Fish => b"exit\n",
				_ => b"exit 2>/dev/null\n",
			};
			let _ = write_tx.send(exit_cmd.into()).await;
		}
	}
}

fn rotate_color(idx: usize) -> u8 {
	const COLORS: [u8; 6] = [31, 32, 33, 34, 35, 36]; // Red, Green, Yellow, Blue, Magenta, Cyan
	// let idx = COLOR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
	COLORS[(idx + 5) % COLORS.len()]
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_remote_state() {
		assert_eq!(RemoteState::NotStarted.as_str(), "not_started");
		assert_eq!(RemoteState::Idle.as_str(), "idle");
	}

	#[test]
	fn test_color_rotation() {
		let c1 = rotate_color(2);
		let c2 = rotate_color(3);
		assert_ne!(c1, c2);
	}
}
