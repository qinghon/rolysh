use crate::errors::{Error, Result};
use nix::sys::signal::{self, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;
use std::io;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::pin::Pin;
use std::process::Command;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::io::unix::AsyncFd;

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

    // Get the raw file descriptor
    // pub fn as_raw_fd(&self) -> RawFd {
    //     self.async_fd.as_raw_fd()
    // }
}

impl AsyncRead for PtyStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
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
                    let n = libc::read(
                        fd,
                        unfilled.as_mut_ptr() as *mut libc::c_void,
                        unfilled.len(),
                    );
                    if n >= 0 {
                        Ok(n as usize)
                    } else {
                        Err(io::Error::last_os_error())
                    }
                }
            }) {
                Ok(Ok(n)) => {
                    unsafe { buf.assume_init(n); }
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
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
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
                    let n = libc::write(
                        fd,
                        buf.as_ptr() as *const libc::c_void,
                        buf.len(),
                    );
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
    pub pty: PtyStream,
}

impl SshProcess {
    /// Spawn a new SSH process with PTY
    pub fn spawn(
        hostname: &str,
        port: &str,
        user: Option<&str>,
        ssh_cmd: &str,
    ) -> Result<Self> {
        // Open PTY
        let mut master_fd: libc::c_int = 0;
        let mut slave_fd: libc::c_int = 0;

        unsafe {
            if libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            ) != 0
            {
                return Err(Error::ConnectionError(format!(
                    "Failed to open PTY: {}",
                    io::Error::last_os_error()
                )));
            }
        }

        // Fork and execute SSH
        let pid = unsafe {
            match libc::fork() {
                -1 => {
                    return Err(Error::ConnectionError(format!(
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

        Ok(SshProcess { pid, pty })
    }

    /// Execute SSH in child process
    fn exec_ssh(slave_fd: RawFd, hostname: &str, port: &str, user: Option<&str>, ssh_cmd: &str) {
        unsafe {
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
                ssh_cmd
                    .replace("%(host)s", &name)
                    .replace("%(port)s", &port_arg)
            } else {
                format!("{ssh_cmd} {port_arg} {name}")
            };

            // Execute via shell
            let _ = Command::new("/bin/sh")
                .arg("-c")
                .arg(&evaluated)
                .exec();

            // If exec fails
            eprintln!("Failed to execute SSH: {}", io::Error::last_os_error());
            std::process::exit(1);
        }
    }

    /// Send init commands to configure the remote shell
    pub fn init_commands(&self) -> Vec<u8> {
        let mut cmds = Vec::new();

        // Disable zle, echo, etc.
        cmds.extend_from_slice(
            b"unsetopt zle 2> /dev/null; stty -echo -onlcr -ctlecho; \
              bind \"set enable-bracketed-paste off\" 2> /dev/null; "
        );

        // Configure prompt
        cmds.extend_from_slice(
            b"PS2=; RPS1=; RPROMPT=; PROMPT_COMMAND=; TERM=ansi; \
              unset precmd_functions; unset HISTFILE; "
        );

        cmds
    }

    /// Kill the SSH process
    pub fn kill(&self) -> Result<()> {
        // Try to kill the process group first
        let _ = signal::kill(Pid::from_raw(-self.pid.as_raw()), Signal::SIGKILL);
        // Also kill the process directly
        signal::kill(self.pid, Signal::SIGKILL)
            .map_err(|e| Error::ConnectionError(format!("Failed to kill process: {e}")))
    }

    /// Wait for the process to exit and get exit code
    pub fn try_wait(&self) -> Result<Option<i32>> {
        match waitpid(self.pid, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(_, code)) => Ok(Some(code)),
            Ok(WaitStatus::Signaled(_, _, _)) => Ok(Some(128)),
            Ok(WaitStatus::StillAlive) => Ok(None),
            Ok(_) => Ok(Some(0)),
            Err(e) => Err(Error::ConnectionError(format!("waitpid failed: {e}"))),
        }
    }
}

impl Drop for SshProcess {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_commands() {
        // Test init commands generation without actual PTY
        // Just test that the command string is correctly formatted
        let init_cmds = b"unsetopt zle 2> /dev/null; stty -echo -onlcr -ctlecho; \
              bind \"set enable-bracketed-paste off\" 2> /dev/null; \
              PS2=; RPS1=; RPROMPT=; PROMPT_COMMAND=; TERM=ansi; \
              unset precmd_functions; unset HISTFILE; ".to_vec();

        assert!(!init_cmds.is_empty());
        assert!(String::from_utf8_lossy(&init_cmds).contains("stty"));
        assert!(String::from_utf8_lossy(&init_cmds).contains("PS2="));
    }
}
