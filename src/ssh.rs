use crate::errors::{Error, Result};
use const_format::concatcp;
use memchr::memchr;
use nix::sys::signal::{self, Signal};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;
use std::io;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::pin::Pin;
use std::process::Command;
use std::str::FromStr;
use std::task::{Context, Poll};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tracing::{debug, error};

pub(crate) const SHELL_DECATE_START: &str = "__ROLYSH_DETECT_START__";
pub(crate) const SHELL_DECATE_END: &str = "__ROLYSH_DETECT_END__";

/// Shell type detected on remote host
#[derive(Debug, Clone, Default, Copy, PartialEq, Eq)]
pub enum ShellType {
	#[default]
	Unknown,
	BashLike, // bash or other POSIX shells
	Zsh,      // zsh
	Fish,
}
impl FromStr for ShellType {
	type Err = Error;
	fn from_str(s: &str) -> Result<Self> {
		match s {
			"unknown" | "Unknown" | "auto" => Ok(ShellType::Unknown),
			"bash" | "Bash" => Ok(ShellType::BashLike),
			"zsh" | "Zsh" => Ok(ShellType::Zsh),
			"fish" | "Fish" => Ok(ShellType::Fish),
			_ => Err(Error::InvalidArgs(format!("Unknown shell type: {s}"))),
		}
	}
}

/// PTY stream that implements AsyncRead and AsyncWrite
/// Wraps a PTY file descriptor for async I/O
pub struct PtyStream {
	async_fd: AsyncFd<OwnedFd>,
}

impl PtyStream {
	/// Create a new PTY stream from a raw file descriptor
	pub fn new(fd: RawFd) -> io::Result<Self> {
		// Set non-blocking mode
		let flags = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
		if flags < 0 {
			return Err(io::Error::last_os_error());
		}
		unsafe {
			if libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
				return Err(io::Error::last_os_error());
			}
		}

		// Wrap in OwnedFd for safe management
		let owned_fd = unsafe { OwnedFd::from_raw_fd(fd) };

		// Create async fd
		let async_fd = AsyncFd::new(owned_fd)?;

		Ok(Self { async_fd })
	}

	/// Configure TTY settings (disable echo, etc.)
	pub fn configure_tty(&self) -> io::Result<()> {
		unsafe {
			let fd = self.async_fd.as_raw_fd();
			let mut attr: libc::termios = std::mem::zeroed();

			if libc::tcgetattr(fd, &mut attr) == 0 {
				// Disable echo and onlcr
				attr.c_oflag &= !libc::ONLCR;
				attr.c_lflag &= !libc::ECHO;
				libc::tcsetattr(fd, libc::TCSANOW, &attr);
			}
		}
		Ok(())
	}

	/// Split the PTY stream into separate read and write halves
	pub fn split(self) -> io::Result<(PtyReadHalf, PtyWriteHalf)> {
		let owned_fd = self.async_fd.into_inner();
		let cloned_fd = owned_fd.try_clone()?;
		let read_half = PtyReadHalf { async_fd: AsyncFd::new(owned_fd)? };
		let write_half = PtyWriteHalf { async_fd: AsyncFd::new(cloned_fd)? };
		Ok((read_half, write_half))
	}

	// Get the raw file descriptor
	// pub fn as_raw_fd(&self) -> RawFd {
	//     self.async_fd.as_raw_fd()
	// }
}

impl AsyncRead for PtyStream {
	fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
		loop {
			let mut guard = match self.async_fd.poll_read_ready(cx) {
				Poll::Ready(Ok(guard)) => guard,
				Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
				Poll::Pending => return Poll::Pending,
			};

			match guard.try_io(|inner| {
				let fd = inner.as_raw_fd();
				// Safety: we're reading into a buffer managed by ReadBuf
				unsafe {
					let unfilled = buf.unfilled_mut();
					let n = libc::read(fd, unfilled.as_mut_ptr() as *mut libc::c_void, unfilled.len());
					if n >= 0 {
						Ok(n as usize)
					} else {
						Err(io::Error::last_os_error())
					}
				}
			}) {
				Ok(Ok(n)) => {
					unsafe {
						buf.assume_init(n);
					}
					buf.advance(n);
					return Poll::Ready(Ok(()));
				}
				Ok(Err(e)) => return Poll::Ready(Err(e)),
				Err(_would_block) => continue,
			}
		}
	}
}

impl AsyncWrite for PtyStream {
	fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
		loop {
			let mut guard = match self.async_fd.poll_write_ready(cx) {
				Poll::Ready(Ok(guard)) => guard,
				Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
				Poll::Pending => return Poll::Pending,
			};

			match guard.try_io(|inner| {
				let fd = inner.as_raw_fd();
				// Safety: we're writing from a valid buffer
				unsafe {
					let n = libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len());
					if n >= 0 {
						Ok(n as usize)
					} else {
						Err(io::Error::last_os_error())
					}
				}
			}) {
				Ok(result) => return Poll::Ready(result),
				Err(_would_block) => continue,
			}
		}
	}

	fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		Poll::Ready(Ok(()))
	}

	fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		Poll::Ready(Ok(()))
	}
}

/// Read half of a PTY stream
pub struct PtyReadHalf {
	pub(crate) async_fd: AsyncFd<OwnedFd>,
}

/// Write half of a PTY stream
pub struct PtyWriteHalf {
	pub(crate) async_fd: AsyncFd<OwnedFd>,
}

impl AsyncRead for PtyReadHalf {
	fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
		loop {
			let mut guard = match self.async_fd.poll_read_ready(cx) {
				Poll::Ready(Ok(guard)) => guard,
				Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
				Poll::Pending => return Poll::Pending,
			};

			match guard.try_io(|inner| {
				let fd = inner.as_raw_fd();
				// Safety: we're reading into a buffer managed by ReadBuf
				unsafe {
					let unfilled = buf.unfilled_mut();
					let n = libc::read(fd, unfilled.as_mut_ptr() as *mut libc::c_void, unfilled.len());
					if n >= 0 {
						Ok(n as usize)
					} else {
						Err(io::Error::last_os_error())
					}
				}
			}) {
				Ok(Ok(n)) => {
					unsafe {
						buf.assume_init(n);
					}
					buf.advance(n);
					return Poll::Ready(Ok(()));
				}
				Ok(Err(e)) => return Poll::Ready(Err(e)),
				Err(_would_block) => continue,
			}
		}
	}
}

impl AsyncWrite for PtyWriteHalf {
	fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
		loop {
			let mut guard = match self.async_fd.poll_write_ready(cx) {
				Poll::Ready(Ok(guard)) => guard,
				Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
				Poll::Pending => return Poll::Pending,
			};

			match guard.try_io(|inner| {
				let fd = inner.as_raw_fd();
				// Safety: we're writing from a valid buffer
				unsafe {
					let n = libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len());

					if n >= 0 {
						Ok(n as usize)
					} else {
						Err(io::Error::last_os_error())
					}
				}
			}) {
				Ok(result) => return Poll::Ready(result),
				Err(_would_block) => continue,
			}
		}
	}

	fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		Poll::Ready(Ok(()))
	}

	fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		Poll::Ready(Ok(()))
	}
}

/// SSH process handle
pub struct SshProcess {
	pub pid: Pid,
	// pub pty_read: PtyReadHalf,
	// pub pty_write: PtyWriteHalf,
}

impl SshProcess {
	/// Spawn a new SSH process with PTY
	pub fn spawn(hostname: &str, port: &str, user: Option<&str>, ssh_cmd: &str) -> Result<(Self, PtyStream)> {
		// Open PTY
		let mut master_fd: libc::c_int = 0;
		let mut slave_fd: libc::c_int = 0;

		unsafe {
			// Use null mut pointers for all platforms
			// *mut T can be passed where *const T is expected, and macOS specifically requires *mut
			let termios_ptr = std::ptr::null_mut() as *mut libc::termios;
			let winsize_ptr = std::ptr::null_mut() as *mut libc::winsize;

			if libc::openpty(
				&mut master_fd,
				&mut slave_fd,
				std::ptr::null_mut(),
				termios_ptr,
				winsize_ptr,
			) != 0
			{
				return Err(Error::Connection(format!(
					"Failed to open PTY: {}",
					io::Error::last_os_error()
				)));
			}
		}

		// Fork and execute SSH
		let pid = unsafe {
			match libc::fork() {
				-1 => {
					return Err(Error::Connection(format!(
						"Fork failed: {}",
						io::Error::last_os_error()
					)));
				}
				0 => {
					// Child process
					Self::exec_ssh(slave_fd, hostname, port, user, ssh_cmd);
					std::process::exit(1);
				}
				child_pid => Pid::from_raw(child_pid),
			}
		};

		// Parent process - close slave FD
		unsafe {
			libc::close(slave_fd);
		}

		// Create PTY stream
		let pty = PtyStream::new(master_fd)?;
		pty.configure_tty()?;
		// let (pty_read, pty_write) = pty.split()?;

		Ok((SshProcess { pid }, pty))
	}

	/// Execute SSH in child process
	fn exec_ssh(slave_fd: RawFd, hostname: &str, port: &str, user: Option<&str>, ssh_cmd: &str) {
		unsafe {
			// 创建新会话，成为进程组组长
			libc::setsid();

			// 设置从设备为控制终端
			#[cfg(target_os = "macos")]
			{
				if libc::ioctl(slave_fd, libc::TIOCSCTTY as libc::c_ulong, 0) < 0 {
					error!("Failed to set control terminal");
				}
			}
			#[cfg(not(target_os = "macos"))]
			{
				if libc::ioctl(slave_fd, libc::TIOCSCTTY, 0) < 0 {
					error!("Failed to set control terminal");
				}
			}
			// Redirect stdin/stdout/stderr to slave PTY
			libc::close(0);
			libc::close(1);
			libc::close(2);
			libc::dup2(slave_fd, 0);
			libc::dup2(slave_fd, 1);
			libc::dup2(slave_fd, 2);
			libc::close(slave_fd);

			// Build SSH command
			let name = if let Some(u) = user {
				format!("{u}@{hostname}")
			} else {
				hostname.to_string()
			};

			let port_arg = if port != "22" {
				format!("-p {port}")
			} else {
				String::new()
			};

			// Format SSH command
			let evaluated = if ssh_cmd.contains("%(host)s") {
				ssh_cmd.replace("%(host)s", &name).replace("%(port)s", &port_arg)
			} else {
				format!("{ssh_cmd} {port_arg} {name}")
			};

			// Execute via shell
			let _ = Command::new("/bin/sh").env("TERM", "ansi").arg("-c").arg(&evaluated).exec();

			// If exec fails
			error!("Failed to execute SSH: {}", io::Error::last_os_error());
			std::process::exit(1);
		}
	}

	/// Get shell detection command
	pub fn shell_detection_command() -> &'static str {
		concatcp!(
			"stty -echo -onlcr -ctlecho; echo ",
			SHELL_DECATE_START,
			" $SHELL ",
			SHELL_DECATE_END,
			"\n"
		)
	}

	/// Send init commands for bash-like shells (bash, zsh)
	pub fn init_commands_bash() -> Vec<u8> {
		let mut cmds = Vec::new();

		// Disable zle, echo, etc.
		cmds.extend_from_slice(
			b"unsetopt zle 2> /dev/null; stty -echo -onlcr -ctlecho; \
              bind \"set enable-bracketed-paste off\" 2> /dev/null; ",
		);

		// Configure prompt
		cmds.extend_from_slice(
			b"PS2=; RPS1=; RPROMPT=; PROMPT_COMMAND=; TERM=ansi; \
              unset precmd_functions; unset HISTFILE; ",
		);

		cmds
	}
	pub fn init_commands_zsh() -> Vec<u8> {
		let cmds = b"stty -echo -onlcr -ctlecho;\
unsetopt zle;\
unsetopt autocd;\
unsetopt autopushd;\
unsetopt correct;\
unsetopt correct_all;\
unsetopt auto_menu;\
unsetopt auto_list;\
unsetopt menu_complete;\
unsetopt list_ambiguous;\
unsetopt complete_in_word;\
PS1='';\
PS2='';\
PS3='';\
PS4='';\
RPS1='';\
RPROMPT='';\
PROMPT='';\
PROMPT2='';\
SPROMPT='';\
zstyle ':completion:*' completer _complete;\
zstyle ':completion:*' use-compctl false;\
zstyle ':completion:*' menu no;\
autoload -Uz compinit && compinit -u;\
compdef -d git 2>/dev/null;\
compdef -d ssh 2>/dev/null;\
compdef -d cd 2>/dev/null;\
unalias -a;\
unfunction compinit 2>/dev/null;\
unfunction _complete 2>/dev/null;\
unfunction _main_complete 2>/dev/null;\
bind 'set enable-bracketed-paste off' 2>/dev/null;\
TERM=ansi;\
unset HISTFILE;\
HISTSIZE=0;\
SAVEHIST=0;\
precmd_functions=();\
preexec_functions=();\
chpwd_functions=();\
() {
  local hook
  for hook in chpwd precmd preexec periodic; do
    add-zsh-hook -D \"${hook}\" \"*\" 2>/dev/null
  done
} 2>/dev/null;\
zle -D self-insert 2>/dev/null;\
zle -D accept-line 2>/dev/null;\
bindkey -d 2>/dev/null;\
unsetopt brace_ccl;\
unsetopt glob;\
unsetopt extended_glob;\
unsetopt nomatch;\
unsetopt notify;\
unsetopt beep;\
";
		cmds.to_vec()
	}

	/// Send init commands for fish shell
	pub fn init_commands_fish() -> Vec<u8> {
		// let mut cmds = Vec::new();
		// set -g fish_key_bindings none
		let cms = b"stty -echo -onlcr -ctlecho;\
printf '\\e[?2004l';\
set -gx TERM ansi;\
set -g fish_greeting '';\
function fish_right_prompt; end;\
function fish_mode_prompt; end;\
set -g fish_history none;\
set -g histfile '/dev/null';\
set -g fish_autosuggestion_enabled 0;\
set -g fish_handle_reflow 0;\
set -g __fish_active_autosuggestions 0;\
set -g fish_complete_path '';\
set -g fish_function_path '';\
set -g fish_features none;\
set -e fish_complete;\
set -e __fish_complete_command;\
set -g fish_color_normal normal;\
set -g fish_color_command normal;\
set -g fish_color_param normal;\
set -g fish_color_comment normal;\
set -g fish_color_error normal;\
set -g fish_color_escape normal;\
set -g fish_color_operator normal;\
set -g fish_color_end normal;\
set -g fish_color_quote normal;\
set -g fish_color_redirection normal;\
set -g fish_color_search_match normal;\
set -g fish_color_valid_path normal;\
";
		cms.to_vec()
	}

	/// Get init commands based on shell type
	pub fn init_commands_for_shell(shell_type: ShellType) -> Vec<u8> {
		match shell_type {
			ShellType::Fish => Self::init_commands_fish(),
			ShellType::Zsh => Self::init_commands_zsh(),
			ShellType::BashLike | ShellType::Unknown => Self::init_commands_bash(),
		}
	}

	/// Parse shell detection output to determine shell type
	pub fn detect_shell_from_output(output: &[u8]) -> Option<ShellType> {
		// Strip ANSI escape codes and convert to lowercase
		// let cleaned = Self::strip_ansi_codes(output);
		let output_str = String::from_utf8_lossy(output);

		debug!("Detection output: {:?}", output_str);

		// Method 1: Check for Fish's welcome message (most reliable)
		if output_str.contains("Welcome to fish") {
			debug!("Detected Fish via welcome message");
			return Some(ShellType::Fish);
		}
		let mut len = 0;
		while let Some(start_idx) = output_str[len..].find(SHELL_DECATE_START) {
			if let Some(end_idx) = output_str[len..].find(SHELL_DECATE_END) {
				if end_idx <= start_idx {
					len = start_idx;
					continue;
				}
				let between = &output_str[len..][start_idx..end_idx];

				debug!("Found markers, content between: {:?} {start_idx}:{end_idx}", between);

				// Look for actual shell path output (contains slashes)
				// This distinguishes real output from command echoes
				if between.contains("/fish") || between.contains("/usr/bin/fish") {
					debug!("Detected Fish via path match");
					return Some(ShellType::Fish);
				} else if between.contains("/zsh") || between.contains("/zsh") {
					return Some(ShellType::Zsh);
				} else if between.contains("/bash") || between.contains("/ash") {
					debug!(
						"Detected BashLike via path match from {:?} {:?} {:?}",
						between,
						between.find("/bash"),
						between.find("/ash")
					);
					return Some(ShellType::BashLike);
				}

				len = end_idx;
			} else {
				break;
			}
		}

		None // Haven't received complete and reliable detection output yet
	}

	/// Kill the SSH process
	pub fn kill(&self) -> Result<()> {
		// Try to kill the process group first
		let _ = signal::kill(Pid::from_raw(-self.pid.as_raw()), Signal::SIGKILL);
		// Also kill the process directly
		signal::kill(self.pid, Signal::SIGKILL).map_err(|e| Error::Connection(format!("Failed to kill process: {e}")))
	}

	/// Wait for the process to exit and get exit code
	pub fn try_wait(&self) -> Result<Option<i32>> {
		match waitpid(self.pid, Some(WaitPidFlag::WNOHANG)) {
			Ok(WaitStatus::Exited(_, code)) => Ok(Some(code)),
			Ok(WaitStatus::Signaled(_, _, _)) => Ok(Some(128)),
			Ok(WaitStatus::StillAlive) => Ok(None),
			Ok(_) => Ok(Some(0)),
			Err(e) => Err(Error::Connection(format!("waitpid failed: {e}"))),
		}
	}
}

impl Drop for SshProcess {
	fn drop(&mut self) {
		let _ = self.kill();
	}
}

pub fn fmt_shell_prompt(shell_type: ShellType, prefix: &str) -> Vec<u8> {
	let mut init_cmds = SshProcess::init_commands_for_shell(shell_type);

	// Set prompt based on shell type
	let prompt_cmd = match shell_type {
		ShellType::Fish => {
			format!("function fish_prompt; printf '{prefix}' ; end;")
		}
		ShellType::Zsh => {
			format!("PROMPT='{prefix}'")
		}
		_ => {
			format!("PS1='{prefix}'")
		}
	};
	init_cmds.extend_from_slice(prompt_cmd.as_bytes());
	init_cmds
}

pub(crate) fn fmt_prompt(id: usize) -> (String, String) {
	let prompt = format!("\\`polysh-{id}/\n");
	let prefix = prompt[1..].to_string();
	(prompt, prefix)
}

pub(crate) fn search_prompt<'a>(prefix: &str, data: &'a [u8]) -> Option<&'a [u8]> {
	if prefix.is_empty() {
		return None;
	}

	let prefix_bytes = prefix.as_bytes();
	let first_byte = prefix_bytes[0];

	// 使用 memchr 快速定位首字节位置
	let mut pos = 0;
	while let Some(idx) = memchr(first_byte, &data[pos..]) {
		let candidate_pos = pos + idx;

		// 检查剩余长度是否足够
		if candidate_pos + prefix_bytes.len() <= data.len() {
			// 验证完整前缀匹配
			if data[candidate_pos..].starts_with(prefix_bytes) {
				let remaining_start = candidate_pos + prefix_bytes.len();
				return Some(&data[remaining_start..]);
			}
		}

		// 继续搜索下一个位置
		pos = candidate_pos + 1;
	}

	None
}

#[cfg(test)]
mod tests {
	use super::*;
	use const_format::concatcp;

	#[test]
	fn test_shell_detection() {
		// Test fish detection
		assert_eq!(
			SshProcess::detect_shell_from_output(
				concatcp!(SHELL_DECATE_START, "\n/usr/bin/fish\n", SHELL_DECATE_END).as_bytes()
			),
			Some(ShellType::Fish)
		);

		// Test bash detection
		assert_eq!(
			SshProcess::detect_shell_from_output(
				concatcp!(SHELL_DECATE_START, "\n/bin/bash\n", SHELL_DECATE_END).as_bytes()
			),
			Some(ShellType::BashLike)
		);

		// Test zsh detection
		assert_eq!(
			SshProcess::detect_shell_from_output(
				concatcp!(SHELL_DECATE_START, "\n/bin/zsh\n", SHELL_DECATE_END).as_bytes()
			),
			Some(ShellType::Zsh)
		);

		// Test incomplete detection
		assert_eq!(SshProcess::detect_shell_from_output(b"some random output"), None);
	}
}
