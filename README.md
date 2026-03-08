# Rolysh

**English** | [中文](README.zh.md)

A modern Rust-based tool for parallel SSH connections. Execute commands across multiple hosts simultaneously with an intuitive syntax and efficient async I/O.

> **Note**: This project is a Rust rewrite of [polysh](https://github.com/innogames/polysh), created to address asynchronous I/O and compatibility issues in the original Python implementation.

## Features

- **Parallel SSH execution**: Run commands on multiple hosts in parallel
- **Host syntax expansion**: Support for range-based hostnames (e.g., `host[01-10]`)
- **Interactive mode**: Interactive shell when no command is provided
- **Non-interactive mode**: Pipe commands via stdin or use `--command` flag
- **Efficient async I/O**: Built on Tokio for high-performance networking
- **Configurable logging**: Debug logging to file or stdout with environment variable control
- **File descriptor limit management**: Automatically adjusts ulimits for large host counts
- **Type-safe state management**: No global mutable state, clean module boundaries
- **polysh compatibility**: Compatible with polysh host syntax and behavior patterns

## Installation

### From Source

```bash
git clone https://github.com/yourusername/rolysh.git
cd rolysh
cargo build --release
sudo cp target/release/rolysh /usr/local/bin/
```

### Cargo Install

```bash
cargo install --git https://github.com/yourusername/rolysh.git
```

## Usage

### Basic Command Execution

```bash
# Run command on multiple hosts
rolysh host1,host2,host3 --command "uptime"

# Use host range syntax
rolysh web[01-10] --command "df -h"

# Interactive mode (no command)
rolysh host1,host2
```

### Host Syntax

- Comma-separated lists: `host1,host2,host3`
- Range expansion: `host[01-10]` expands to `host01`, `host02`, ..., `host10`
- Mixed syntax: `host[1-3],server1,server2`

### Options

```
--command, -c      Command to execute on remote hosts
--debug, -d        Enable debug logging
--log-file         Specify log file path (default: /tmp/rolysh.log)
--help, -h         Show help message
```

### Examples

```bash
# Check disk usage across web servers
rolysh web[01-05] --command "df -h | grep /dev/sda1"

# Run interactive shell on two hosts
rolysh host1,host2

# Pipe command from stdin
echo "ls -la" | rolysh host1,host2

# With debug logging
rolysh host[1-10] --command "whoami" --debug
```

## Building

```bash
cargo build
cargo build --release
```

## Testing

```bash
cargo test
```

## License

MIT License - see [LICENSE](LICENSE) file for details.