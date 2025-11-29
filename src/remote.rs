use crate::async_io::LineReader;
use crate::callbacks::CallbackManager;
use crate::errors::{Error, Result};
use crate::ssh::SshProcess;
use std::io::IsTerminal;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::select;
use tokio::sync::mpsc;

/// Remote connection state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// Commands that can be sent to a remote connection
#[derive(Debug, Clone)]
pub enum RemoteCommand {
    /// Send data to remote
    Send(Vec<u8>),
    /// Execute a command
    Execute(String),
    /// Close the connection
    Close,
    /// Enable/disable the connection
    SetEnabled(bool),
    /// Rename the display name
    Rename(String),
}

/// Events emitted by remote connections
#[derive(Debug, Clone)]
pub enum RemoteEvent {
    /// Connection established
    Connected { hostname: String },
    /// State changed
    StateChanged {
        hostname: String,
        state: RemoteState,
    },
    /// Output received
    Output { hostname: String, data: Vec<u8> },
    /// Connection closed
    Closed { hostname: String, exit_code: i32 },
    /// Error occurred
    Error { hostname: String, error: String },
}

/// Handle to communicate with a remote connection task
pub struct RemoteHandle {
    pub hostname: String,
    cmd_tx: mpsc::Sender<RemoteCommand>,
}

impl RemoteHandle {
    pub fn new(hostname: String, cmd_tx: mpsc::Sender<RemoteCommand>) -> Self {
        Self { hostname, cmd_tx }
    }

    /// Send a command to the remote
    pub async fn send_command(&self, cmd: RemoteCommand) -> Result<()> {
        self.cmd_tx
            .send(cmd)
            .await
            .map_err(|_| Error::ConnectionError("Remote task closed".into()))
    }

    /// Send data to remote
    pub async fn send(&self, data: Vec<u8>) -> Result<()> {
        self.send_command(RemoteCommand::Send(data)).await
    }

    /// Execute a command on remote
    pub async fn execute(&self, command: String) -> Result<()> {
        self.send_command(RemoteCommand::Execute(command)).await
    }

    /// Close the connection
    pub async fn close(&self) -> Result<()> {
        self.send_command(RemoteCommand::Close).await
    }

    /// Set enabled state
    pub async fn set_enabled(&self, enabled: bool) -> Result<()> {
        self.send_command(RemoteCommand::SetEnabled(enabled)).await
    }
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
    pub debug: bool,
    pub disable_color: bool,
}

/// Remote connection task
pub struct Remote {
    id: usize,
    config: RemoteConfig,
    state: RemoteState,
    enabled: bool,
    display_name: String,
    color_code: Option<u8>,
    ssh_process: Option<SshProcess>,
    line_reader: LineReader,
    callbacks: CallbackManager,
    init_sent: bool,
    last_printed_line: Vec<u8>,
    read_in_not_started: Vec<u8>,
    prompt_detected: bool,
}

impl Remote {
    /// Create a new remote connection
    pub fn new(id: usize,config: RemoteConfig, callbacks: CallbackManager) -> Self {
        let display_name = config.hostname.clone();

        // Assign color if enabled
        let color_code = if !config.disable_color && std::io::stdout().is_terminal() {
            Some(rotate_color())
        } else {
            None
        };

        Self {
            id,
            config,
            state: RemoteState::NotStarted,
            enabled: true,
            display_name,
            color_code,
            ssh_process: None,
            line_reader: LineReader::new(),
            callbacks,
            init_sent: false,
            last_printed_line: Vec::new(),
            read_in_not_started: Vec::new(),
            prompt_detected: false,
        }
    }

    /// Run the remote connection task
    pub async fn run(
        mut self,
        mut cmd_rx: mpsc::Receiver<RemoteCommand>,
        event_tx: mpsc::Sender<RemoteEvent>,
    ) -> Result<i32> {
        // Establish SSH connection
        let ssh_process = match SshProcess::spawn(
            &self.config.hostname,
            &self.config.port,
            self.config.user.as_deref(),
            &self.config.ssh_cmd,
        ) {
            Ok(ssh) => ssh,
            Err(e) => {
                let _ = event_tx
                    .send(RemoteEvent::Error {
                        hostname: self.config.hostname.clone(),
                        error: format!("Failed to spawn SSH: {e}"),
                    })
                    .await;
                return Err(e);
            }
        };

        self.ssh_process = Some(ssh_process);

        // Emit connected event
        let _ = event_tx
            .send(RemoteEvent::Connected {
                hostname: self.config.hostname.clone(),
            })
            .await;

        self.change_state(RemoteState::Connecting, &event_tx).await;

        let mut read_buf = vec![0u8; 4096];
        let mut exit_code = 0;

        // Main event loop
        loop {
            select! {
                // Read from SSH
                result = self.ssh_process.as_mut().unwrap().pty.read(&mut read_buf) => {
                    match result {
                        Ok(0) => {
                            // EOF - connection closed
                            if self.config.debug {
                                self.print_debug(b"EOF received");
                            }
                            break;
                        }
                        Ok(n) => {
                            if self.config.debug {
                                self.print_debug(b"==> ");
                                self.print_debug(&read_buf[..n]);
                            }
                            self.handle_read(&read_buf[..n], &event_tx).await;

                            // CRITICAL FIX: If prompt was detected, drain all remaining PTY data
                            // before changing state. This ensures all output arrives before the prompt.
                            if self.prompt_detected && self.state == RemoteState::Running {
                                if self.config.debug {
                                    eprintln!("[{}] Prompt detected, draining PTY...", self.display_name);
                                }

                                // Drain PTY with timeout
                                loop {
                                    match tokio::time::timeout(
                                        tokio::time::Duration::from_millis(20),
                                        self.ssh_process.as_mut().unwrap().pty.read(&mut read_buf)
                                    ).await {
                                        Ok(Ok(n)) if n > 0 => {
                                            if self.config.debug {
                                                eprintln!("[{}] Drained {} more bytes", self.display_name, n);
                                            }
                                            self.handle_read(&read_buf[..n], &event_tx).await;
                                        }
                                        _ => break,
                                    }
                                }

                                if self.config.debug {
                                    eprintln!("[{}] PTY drained, changing state to Idle", self.display_name);
                                }
                                self.prompt_detected = false;
                                self.change_state(RemoteState::Idle, &event_tx).await;
                            }
                        }
                        Err(e) => {
                            if self.config.debug {
                                eprintln!("[{}] Read error: {}", self.display_name, e);
                            }
                            break;
                        }
                    }
                }

                // Handle commands
                Some(cmd) = cmd_rx.recv() => {
                    match cmd {
                        RemoteCommand::Send(data) => {
                            if self.enabled {
                                let _ = self.write_data(&data).await;
                            }
                        }
                        RemoteCommand::Execute(command) => {
                            if self.enabled && self.state == RemoteState::Idle {
                                // IMPORTANT: Change state BEFORE sending PS1
                                // Otherwise the prompt marker will be treated as output
                                self.change_state(RemoteState::Running, &event_tx).await;

                                // Don't add new callback - just send the command
                                // The callback was already set up during initialization
                                let cmd_bytes = format!("{command}\n").into_bytes();
                                let _ = self.write_data(&cmd_bytes).await;
                            }
                        }
                        RemoteCommand::Close => {
                            break;
                        }
                        RemoteCommand::SetEnabled(enabled) => {
                            self.enabled = enabled;
                        }
                        RemoteCommand::Rename(name) => {
                            self.display_name = name;
                        }
                    }
                }

                else => break,
            }
        }

        // Wait for process to exit
        if let Some(ssh) = &self.ssh_process {
            if let Ok(Some(code)) = ssh.try_wait() {
                exit_code = code;
            }
        }

        self.change_state(RemoteState::Terminated, &event_tx).await;

        // Emit closed event
        let _ = event_tx
            .send(RemoteEvent::Closed {
                hostname: self.config.hostname.clone(),
                exit_code,
            })
            .await;

        Ok(exit_code)
    }

    /// Handle incoming data
    async fn handle_read(&mut self, data: &[u8], event_tx: &mpsc::Sender<RemoteEvent>) {
        // Send init commands if not sent yet
        if !self.init_sent && self.state == RemoteState::Connecting {
            let init_cmds = self.ssh_process.as_ref().unwrap().init_commands();
            let _ = self.write_data(&init_cmds).await;

            // Set up prompt callback (NOT one-shot - can trigger multiple times)
            let (p1, p2) = self.callbacks.add(self.id, "prompt", |_| {}, false);
            let prompt_cmd = format!(
                "PS1=\"{}\"\"{}\"\n",
                String::from_utf8_lossy(&p1),
                String::from_utf8_lossy(&p2)
            );
            let _ = self.write_data(prompt_cmd.as_bytes()).await;

            self.init_sent = true;
        }

        // Process lines
        let lines = self.line_reader.add_data(data);

        // Track if we should send output for lines in this batch
        // Even if state changes to Idle mid-batch, we still send lines before prompt
        let was_running = self.state == RemoteState::Running;

        if self.config.debug && !lines.is_empty() {
            eprintln!("[{}] Processing {} lines, was_running={}, state={}",
                self.display_name, lines.len(), was_running, self.state.as_str());
        }

        // Track if we found a prompt in this batch - delay state change until after processing all lines
        let mut prompt_found = false;

        for (idx, line) in lines.iter().enumerate() {
            if self.config.debug {
                eprintln!("[{}] Line {}: {:?}",
                    self.display_name, idx, String::from_utf8_lossy(line));
            }

            // Check for password prompt in Connecting state
            if self.state == RemoteState::Connecting {
                let line_str = String::from_utf8_lossy(line).to_lowercase();
                if line_str.contains("password:") && self.config.password.is_some() {
                    let pwd = self.config.password.as_ref().unwrap();
                    let _ = self.write_data(format!("{pwd}\n").as_bytes()).await;
                    continue;
                }

                // Check for SSH errors
                if line_str.contains("the authenticity of host")
                    || line_str.contains("host identification has changed")
                {
                    let _ = event_tx
                        .send(RemoteEvent::Error {
                            hostname: self.config.hostname.clone(),
                            error: String::from_utf8_lossy(line).to_string(),
                        })
                        .await;
                    self.read_in_not_started.extend_from_slice(line);
                    continue;
                }
            }

            // Always check for callbacks, regardless of state
            // This ensures we detect the "command complete" prompt even if state already transitioned
            if let Some(remaining_after_prompt) = self.callbacks.process(line) {
                // Prompt detected - the marker may be in middle of line
                if self.config.debug {
                    eprintln!("[{}] Prompt detected at line {}/{}, remaining after prompt: {:?}, state={}",
                        self.display_name, idx, lines.len(),
                        String::from_utf8_lossy(&remaining_after_prompt), self.state.as_str());
                }



                // If there's content after the prompt marker in this line, send it as output
                if !remaining_after_prompt.is_empty() && was_running {
                    let _ = event_tx
                        .send(RemoteEvent::Output {
                            hostname: self.config.hostname.clone(),
                            data: remaining_after_prompt.to_vec(),
                        })
                        .await;
                }

                // Mark that we found a prompt - will handle state transition after processing all lines
                prompt_found = true;

                // Handle state transitions for Connecting state immediately
                if self.state == RemoteState::Connecting {
                    self.handle_prompt_seen(event_tx).await;
                }

                // Continue processing remaining lines in this batch as they are still part of the command output
                continue;
            }

            // Send output in IDLE or RUNNING states (matching polysh behavior)
            // This ensures output is sent even after state transitions
            if self.state == RemoteState::Idle || self.state == RemoteState::Running {
                if self.config.debug {
                    eprintln!("[{}] Sending output (state={}): {:?}",
                        self.display_name, self.state.as_str(), String::from_utf8_lossy(line));
                }
                let _ = event_tx
                    .send(RemoteEvent::Output {
                        hostname: self.config.hostname.clone(),
                        data: line.clone(),
                    })
                    .await;
                self.last_printed_line = line.clone();
            } else if self.state == RemoteState::Connecting {
                self.read_in_not_started.extend_from_slice(line);
            }
        }

        // After processing all lines, mark if prompt was found
        // Don't change state immediately - let main loop handle it after draining PTY
        self.prompt_detected = prompt_found;
    }

    /// Handle prompt seen for the first time
    async fn handle_prompt_seen(&mut self, event_tx: &mpsc::Sender<RemoteEvent>) {
        if self.config.interactive {
            self.change_state(RemoteState::Idle, event_tx).await;
        } else if let Some(ref cmd) = self.config.command.clone() {
            // In non-interactive mode, send command and exit
            self.change_state(RemoteState::Running, event_tx).await;

            let (p1, p2) = self.callbacks.add(self.id, "real prompt", |_| {}, true);
            let prompt_cmd = format!(
                "PS1=\"{}\"\"{}\\n\"\n",
                String::from_utf8_lossy(&p1),
                String::from_utf8_lossy(&p2)
            );
            let _ = self.write_data(prompt_cmd.as_bytes()).await;
            let _ = self.write_data(format!("{cmd}\n").as_bytes()).await;
            let _ = self.write_data(b"exit 2>/dev/null\n").await;
        }
    }

    /// Change state and emit event
    async fn change_state(&mut self, new_state: RemoteState, event_tx: &mpsc::Sender<RemoteEvent>) {
        if new_state != self.state {
            if self.config.debug {
                self.print_debug(format!("state => {}", new_state.as_str()).as_bytes());
            }
            self.state = new_state;

            let _ = event_tx
                .send(RemoteEvent::StateChanged {
                    hostname: self.config.hostname.clone(),
                    state: new_state,
                })
                .await;
        }
    }

    /// Write data to SSH process
    async fn write_data(&mut self, data: &[u8]) -> Result<()> {
        if self.config.debug {
            self.print_debug(b"<== ");
            self.print_debug(data);
        }

        if let Some(ref mut ssh) = self.ssh_process {
            ssh.pty.write_all(data).await?;
        }
        Ok(())
    }

    /// Print debug message
    fn print_debug(&self, msg: &[u8]) {
        print!("[dbg] {}[{}]: ", self.display_name, self.state.as_str());
        let _ = std::io::Write::write_all(&mut std::io::stdout(), msg);
        println!();
    }
}

// Global color rotation (simple version)
static COLOR_COUNTER: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

fn rotate_color() -> u8 {
    let colors = [31, 32, 33, 34, 35, 36]; // Red, Green, Yellow, Blue, Magenta, Cyan
    let idx = COLOR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    colors[(idx % colors.len() as u8) as usize]
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
        let c1 = rotate_color();
        let c2 = rotate_color();
        assert_ne!(c1, c2);
    }
}
