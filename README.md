# Rolysh

**English** | [中文](README.zh.md)

A modern Rust-based tool for parallel SSH connections. Execute commands on many hosts simultaneously with an intuitive host syntax and efficient async I/O.

> **Note**: This project is a Rust rewrite of [polysh](https://github.com/innogames/polysh), created to address asynchronous I/O and compatibility issues in the original Python implementation.

## Quick Start

```bash
# Connect to a few hosts (each host is a separate argument)
rolysh host1 host2

# Connect to all hosts in a list file
rolysh --hosts ip.list
```

## Features

- **Parallel SSH execution**: Run commands or shells on many hosts at the same time
- **Host range expansion**: `host<01-10>` expands to `host01`, `host02`, ..., `host10`
- **Interactive mode**: A line-editor shell (with history) when no command is given
- **Non-interactive mode**: Pipe commands via stdin or use the `--command` flag
- **Custom SSH command**: Plug in any SSH client via `--ssh`
- **Remote shell detection**: Auto-detects bash/zsh/fish, or force one with `--force-shell`
- **Efficient async I/O**: Built on Tokio, no polling
- **Auto fd-limit bumping**: Raises the file descriptor ulimit when needed for large host counts
- **Configurable logging**: `--debug` / `RUST_LOG`, with `--log-file` for output to file
- **Colorized output**: Each line is prefixed with its hostname; disable with `--no-color`

## Requirements

- **nightly** Rust toolchain (the crate uses `#![feature(bstr)]`; a `rust-toolchain.toml` pinning nightly is included)
- An `ssh` client on the local system (or any replacement passed via `--ssh`)

## Installation

### From Source

```bash
git clone https://github.com/yourusername/rolysh.git
cd rolysh
cargo +nightly build --release
sudo cp target/release/rolysh /usr/local/bin/
```

### Cargo Install

```bash
cargo +nightly install --git https://github.com/yourusername/rolysh.git
```

## Usage

```
rolysh [OPTIONS] HOSTS...
```

### Quick Examples

```bash
# Run a command on a range of hosts and exit
rolysh web<01-05> -c "df -h"

# Run a command on arbitrary hosts (each host is a separate argument)
rolysh host1 host2 db.prod --command "uptime"

# Interactive mode: no -c, stdin is a terminal -> shared shell
rolysh web<01-05>

# Pipe a command from stdin
echo "ls -la" | rolysh host1 host2

# Connect as another user, aborting on connection errors
rolysh -u deploy --abort-errors web<01-10> -c "systemctl status nginx"

# Read hosts from a file (comments with # are skipped)
rolysh --hosts-file hosts.txt -c "uname -a"

# Log everything to a file with debug output
rolysh host1 host2 -c "whoami" --debug --log-file /tmp/rolysh.log
```

### Host Syntax

- Each host is a **separate argument** on the command line (no comma lists):
  `rolysh host1 host2 host3`
- Range expansion: `web<01-10>` expands to `web01` ... `web10`.
  Zero-padding is taken from the start of the range: `host<1-5>` → `host1`..`host5`,
  `host<01-5>` → `host01`..`host05`.
- Explicit port: `host:2222`. IPv6 addresses use brackets: `[fe80::1]:2222`.
- If the same host appears twice, the second and later occurrences are shown
  as `host#1`, `host#2`, ... in the output.

### Options

```
--version                      Show version
-h, --help                     Show this help message
--hosts-file|--hosts FILE      Read hostnames from a file (# comments supported)
--command|-c CMD               Execute command and exit (non-interactive)
--ssh SSH                      SSH command to use
                               (default: exec ssh -oLogLevel=Quiet -t %(host)s %(port)s)
--user|-u USER                 Remote user to log in as
--no-color                     Disable colored hostnames
--password-file FILE           Read password from file (- for stdin)
--password|-p 'PASSWD'         Set password from the CLI
--log-file FILE                Log all I/O to a file (used with --debug)
--separator|--sep SEP          Separator after hostname in output (default: :)
--abort-errors                 Exit on connection errors
--debug                        Enable debug output
--force-shell SHELL            Force remote shell type: bash, zsh, fish, auto
```

The `--ssh` command template receives `%(host)s` and `%(port)s` substitutions, e.g.:

```bash
rolysh --ssh "ssh -i ~/.ssh/deploy_ed25519 -t %(host)s %(port)s" host1 -c "id"
```

### Interactive Mode Control Commands

In interactive mode, lines starting with `:` are local control commands, and lines
starting with `!` run a command in your **local** shell (`sh -c`):

```
:list, :l             List all remotes and their states
:enable [hosts...]    Enable remotes (all if no arguments)
:disable [hosts...]   Disable remotes (all if no arguments)
:quit, :q, :exit      Quit the session
:help, :h             Show control-command help
!ls -la               Run `ls -la` locally, not on the remote hosts
```

Any other line is sent to all enabled remote shells. The prompt shows how many
remotes are still waiting for you, e.g. `wait (2/3)> ` or `ready (3)> `.
Command history is stored in `~/.rolysh_history`.

### Batch Mode

With `--command` (or piped stdin), rolysh runs the command on every host,
prints the combined output, and exits. The exit status is the maximum exit
code of the remote commands. Note that `--command` and reading a command from
stdin are mutually exclusive.

### Logging

- By default logs go to stdout at `INFO` level.
- `--debug` raises the level to `DEBUG` and (if no `--log-file` is given)
  writes the log to `/tmp/rolysh.log`.
- The level can be overridden with the `RUST_LOG` environment variable,
  e.g. `RUST_LOG=rolysh=trace rolysh ...`.

## Building

```bash
cargo +nightly build
cargo +nightly build --release
```

## Testing

```bash
cargo +nightly test
```

## License

MIT License - see [LICENSE](LICENSE) file for details.
