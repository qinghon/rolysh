use crate::callbacks::CallbackManager;
use crate::config::Config;
use crate::errors::Result;
use crate::remote::{Remote, RemoteConfig, RemoteEvent, RemoteHandle, RemoteState};
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::collections::HashMap;
use std::io::{self, IsTerminal};
use tokio::select;
use tokio::sync::mpsc;

/// Session manager coordinates multiple remote connections
pub struct SessionManager {
    config: Config,
    remotes: HashMap<String, RemoteHandle>,
    event_rx: mpsc::Receiver<RemoteEvent>,
    event_tx: mpsc::Sender<RemoteEvent>,
    remote_states: HashMap<String, RemoteState>,
    exit_code: i32,
}

impl SessionManager {
    /// Create a new session manager
    pub async fn new(config: Config, hosts: Vec<String>) -> Result<Self> {
        let (event_tx, event_rx) = mpsc::channel(1000);
        let mut remotes = HashMap::new();
        let mut remote_states = HashMap::new();

        // Spawn a task for each host
        for (id, host) in hosts.iter().enumerate() {
            let (hostname, port) = parse_host_port(&host);

            let remote_config = RemoteConfig {
                hostname: hostname.clone(),
                port: port.clone(),
                user: config.user.clone(),
                ssh_cmd: config.ssh_cmd.clone(),
                password: config.password.clone(),
                command: config.command.clone(),
                interactive: config.interactive,
                debug: config.debug,
                disable_color: config.disable_color,
            };

            let callbacks = CallbackManager::new();
            let remote = Remote::new(id, remote_config, callbacks);

            let (cmd_tx, cmd_rx) = mpsc::channel(100);
            let event_tx_clone = event_tx.clone();

            // Spawn remote task
            tokio::spawn(async move {
                let _ = remote.run(cmd_rx, event_tx_clone).await;
            });

            let handle = RemoteHandle::new(hostname.clone(), cmd_tx);
            remotes.insert(hostname.clone(), handle);
            remote_states.insert(hostname.clone(), RemoteState::NotStarted);
        }

        Ok(Self {
            config,
            remotes,
            event_rx,
            event_tx,
            remote_states,
            exit_code: 0,
        })
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
        // Channel for readline results
        let (stdin_tx, mut stdin_rx) = mpsc::channel::<Option<String>>(1);
        let mut pending_readline: Option<tokio::task::JoinHandle<()>> = None;
        let mut need_input = true; // Start by requesting input

        loop {
            // Request input if needed and not already waiting
            // With proper prompt detection, we can trust the state transitions
            if need_input && pending_readline.is_none() && self.all_idle_or_terminated() {
                // Simple drain of any pending events before starting readline
                loop {
                    match self.event_rx.try_recv() {
                        Ok(event) => {
                            let is_state_change = matches!(
                                event,
                                RemoteEvent::StateChanged { .. } | RemoteEvent::Closed { .. }
                            );
                            self.handle_event(event).await;
                            if is_state_change && !self.all_idle_or_terminated() {
                                need_input = false;
                            }
                        }
                        Err(_) => break, // No more events
                    }
                }

                // Only start readline if still ready after draining
                if need_input && self.all_idle_or_terminated() {
                    let prompt = self.get_prompt_string();
                    let tx = stdin_tx.clone();
                    let handle = tokio::task::spawn_blocking(move || {
                        let mut editor = match DefaultEditor::new() {
                            Ok(e) => e,
                            Err(_) => {
                                let _ = tx.blocking_send(None);
                                return;
                            }
                        };

                        match editor.readline(&prompt) {
                            Ok(line) => {
                                let _ = editor.add_history_entry(&line);
                                let _ = tx.blocking_send(Some(line));
                            }
                            Err(ReadlineError::Eof) => {
                                let _ = tx.blocking_send(Some("\x04".to_string()));
                            }
                            Err(ReadlineError::Interrupted) => {
                                let _ = tx.blocking_send(None);
                            }
                            Err(_) => {
                                let _ = tx.blocking_send(None);
                            }
                        }
                    });
                    pending_readline = Some(handle);
                    need_input = false;
                }
            }

            select! {
                // Handle stdin
                Some(line_opt) = stdin_rx.recv() => {
                    pending_readline = None;

                    // Handle the line if it exists
                    if let Some(line) = line_opt {
                        let line = line.trim();

                        // Check for Ctrl+D (EOF marker)
                        if line == "\x04" {
                            // Forward Ctrl+D to all enabled remotes
                            for (hostname, handle) in &self.remotes {
                                if let Some(&state) = self.remote_states.get(hostname) {
                                    if state == RemoteState::Idle {
                                        let _ = handle.send(vec![0x04]).await;
                                    }
                                }
                            }
                            need_input = true;
                            continue;
                        }

                        if line.is_empty() {
                            need_input = true;
                            continue;
                        }

                        if line.starts_with(':') {
                            self.handle_control_command(&line[1..]).await?;
                            need_input = true;
                        } else if line.starts_with('!') {
                            let _ = tokio::process::Command::new("sh")
                                .arg("-c")
                                .arg(&line[1..])
                                .status()
                                .await;
                            need_input = true;
                        } else {
                            // Remote command - will request input when all become idle
                            self.send_to_all_enabled(line).await?;
                        }
                    } else {
                        // Interrupted
                        need_input = true;
                    }
                }

                // Handle remote events
                Some(event) = self.event_rx.recv() => {
                    // Check if we should request input after this event
                    let is_state_change = matches!(event, RemoteEvent::StateChanged { .. } | RemoteEvent::Closed { .. });

                    self.handle_event(event).await;

                    // Request input when all remotes become idle (only on state changes)
                    // Remote tasks now drain their PTYs before sending StateChanged,
                    // so we can trust that all output has been sent
                    if is_state_change && self.all_idle_or_terminated() {
                        // Still do a quick drain of any buffered events in the channel
                        while let Ok(event) = self.event_rx.try_recv() {
                            self.handle_event(event).await;
                        }
                        need_input = true;
                    }
                }

                else => break,
            }

            // Check if all remotes are terminated
            if self.all_terminated() {
                break;
            }
        }

        Ok(())
    }

    /// Run in batch mode
    async fn run_batch(&mut self) -> Result<()> {
        // Wait for all remotes to complete
        loop {
            if let Some(event) = self.event_rx.recv().await {
                self.handle_event(event).await;

                if self.all_terminated() {
                    break;
                }
            } else {
                break;
            }
        }

        Ok(())
    }

    /// Handle a remote event
    async fn handle_event(&mut self, event: RemoteEvent) {
        match event {
            RemoteEvent::Connected { hostname } => {
                if self.config.debug {
                    eprintln!("[{hostname}] Connected");
                }
            }
            RemoteEvent::StateChanged { hostname, state } => {
                self.remote_states.insert(hostname.clone(), state);
                if self.config.debug {
                    eprintln!("[{}] State: {}", hostname, state.as_str());
                }
            }
            RemoteEvent::Output { hostname, data } => {
                self.print_remote_output(&hostname, &data);
            }
            RemoteEvent::Closed {
                hostname,
                exit_code,
            } => {
                self.remote_states
                    .insert(hostname.clone(), RemoteState::Terminated);
                if exit_code != 0 {
                    self.exit_code = self.exit_code.max(exit_code);
                    if self.config.interactive {
                        eprintln!("[{hostname}] Exited with code {exit_code}");
                    }
                }
            }
            RemoteEvent::Error { hostname, error } => {
                eprintln!("[{hostname}] Error: {error}");
            }
        }
    }

    /// Send command to all enabled remotes
    async fn send_to_all_enabled(&mut self, command: &str) -> Result<()> {
        for (hostname, handle) in &self.remotes {
            if let Some(&state) = self.remote_states.get(hostname) {
                if state == RemoteState::Idle {
                    handle.execute(command.to_string()).await?;
                }
            }
        }
        Ok(())
    }

    /// Handle control commands (:list, :quit, etc.)
    async fn handle_control_command(&mut self, cmd: &str) -> Result<()> {
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        if parts.is_empty() {
            return Ok(());
        }

        match parts[0] {
            "list" | "l" => {
                println!("Remotes:");
                for hostname in self.remotes.keys() {
                    let state = self
                        .remote_states
                        .get(hostname)
                        .unwrap_or(&RemoteState::NotStarted);
                    println!("  {} - {}", hostname, state.as_str());
                }
            }
            "quit" | "q" | "exit" => {
                // Close all remotes
                for handle in self.remotes.values() {
                    let _ = handle.close().await;
                }
                std::process::exit(self.exit_code);
            }
            "enable" | "e" => {
                if parts.len() > 1 {
                    for hostname in &parts[1..] {
                        if let Some(handle) = self.remotes.get(*hostname) {
                            let _ = handle.set_enabled(true).await;
                            println!("Enabled {hostname}");
                        }
                    }
                } else {
                    // Enable all
                    for handle in self.remotes.values() {
                        let _ = handle.set_enabled(true).await;
                    }
                    println!("Enabled all");
                }
            }
            "disable" | "d" => {
                if parts.len() > 1 {
                    for hostname in &parts[1..] {
                        if let Some(handle) = self.remotes.get(*hostname) {
                            let _ = handle.set_enabled(false).await;
                            println!("Disabled {hostname}");
                        }
                    }
                } else {
                    // Disable all
                    for handle in self.remotes.values() {
                        let _ = handle.set_enabled(false).await;
                    }
                    println!("Disabled all");
                }
            }
            "help" | "h" => {
                println!("Control commands:");
                println!("  :list, :l          - List all remotes and their states");
                println!("  :quit, :q, :exit   - Quit the session");
                println!("  :enable [hosts...] - Enable remotes (all if no args)");
                println!("  :disable [hosts...] - Disable remotes (all if no args)");
                println!("  :help, :h          - Show this help");
            }
            _ => {
                println!("Unknown command: {}", parts[0]);
                println!("Type :help for available commands");
            }
        }

        Ok(())
    }

    /// Print remote output with hostname prefix
    fn print_remote_output(&self, hostname: &str, data: &[u8]) {
        // Format output with color if enabled
        let prefix = if !self.config.disable_color && io::stdout().is_terminal() {
            // Simple color cycling (could be improved)
            format!("\x1b[1;32m[{hostname}]\x1b[0m ")
        } else {
            format!("[{hostname}] ")
        };

        // Process output similar to polysh:
        // 1. Strip trailing newlines
        // 2. Replace all \n with \n + prefix
        let output = String::from_utf8_lossy(data);
        let trimmed = output.trim_end_matches('\n');

        if trimmed.is_empty() {
            return;
        }

        // Replace newlines with newline + prefix
        let prefixed = trimmed.replace('\n', &format!("\n{prefix}"));

        // Print the output (rustyline handles prompt redrawing in its own thread)
        println!("{prefix}{prefixed}");
    }

    /// Get the prompt string based on current state
    fn get_prompt_string(&self) -> String {
        // Count non-idle and non-terminated connections (awaited)
        // Following polysh logic: any state that is not IDLE is "awaited"
        let waiting = self
            .remote_states
            .values()
            .filter(|&&s| s != RemoteState::Idle && s != RemoteState::Terminated)
            .count();

        // Total is the number of non-terminated connections
        let total = self
            .remote_states
            .values()
            .filter(|&&s| s != RemoteState::Terminated)
            .count();

        if waiting > 0 {
            format!("waiting ({waiting}/{total})> ")
        } else {
            format!("ready ({total})> ")
        }
    }

    /// Check if all remotes are terminated
    fn all_terminated(&self) -> bool {
        self.remote_states
            .values()
            .all(|&s| s == RemoteState::Terminated)
    }

    /// Check if all remotes are idle or terminated (no commands running)
    fn all_idle_or_terminated(&self) -> bool {
        self.remote_states
            .values()
            .all(|&s| s == RemoteState::Idle || s == RemoteState::Terminated)
    }
}

/// Parse host:port format
fn parse_host_port(host: &str) -> (String, String) {
    if let Some(colon_idx) = host.rfind(':') {
        let (hostname, port_str) = host.split_at(colon_idx);
        (hostname.to_string(), port_str[1..].to_string())
    } else {
        (host.to_string(), "22".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_host_port() {
        assert_eq!(
            parse_host_port("example.com"),
            ("example.com".to_string(), "22".to_string())
        );
        assert_eq!(
            parse_host_port("example.com:2222"),
            ("example.com".to_string(), "2222".to_string())
        );
    }
}
