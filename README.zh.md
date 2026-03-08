# Rolysh

[English](README.md) | **中文**

一个基于 Rust 的现代化并行 SSH 连接工具。使用直观的语法和高效的异步 I/O 同时在多台主机上执行命令。

> **注意**：本项目是 [polysh](https://github.com/innogames/polysh) 的 Rust 重写版本，旨在解决原 Python 实现中的异步 I/O 和兼容性问题。

## 功能特性

- **并行 SSH 执行**：在多台主机上并行运行命令
- **主机语法扩展**：支持基于范围的主机名（例如 `host[01-10]`）
- **交互模式**：未提供命令时进入交互式 shell
- **非交互模式**：通过 stdin 管道传递命令或使用 `--command` 标志
- **高效异步 I/O**：基于 Tokio 构建，实现高性能网络通信
- **可配置日志**：通过环境变量控制调试日志输出到文件或 stdout
- **文件描述符限制管理**：自动调整 ulimits 以支持大量主机连接
- **类型安全的状态管理**：无全局可变状态，清晰的模块边界
- **polysh 兼容性**：兼容 polysh 的主机语法和行为模式

## 安装

### 从源码安装

```bash
git clone https://github.com/yourusername/rolysh.git
cd rolysh
cargo build --release
sudo cp target/release/rolysh /usr/local/bin/
```

### 通过 Cargo 安装

```bash
cargo install --git https://github.com/yourusername/rolysh.git
```

## 使用方法

### 基本命令执行

```bash
# 在多台主机上运行命令
rolysh host1,host2,host3 --command "uptime"

# 使用主机范围语法
rolysh web[01-10] --command "df -h"

# 交互模式（无命令）
rolysh host1,host2
```

### 主机语法

- 逗号分隔列表：`host1,host2,host3`
- 范围扩展：`host[01-10]` 扩展为 `host01`, `host02`, ..., `host10`
- 混合语法：`host[1-3],server1,server2`

### 选项

```
--command, -c      在远程主机上执行的命令
--debug, -d        启用调试日志
--log-file         指定日志文件路径（默认：/tmp/rolysh.log）
--help, -h         显示帮助信息
```

### 示例

```bash
# 检查 Web 服务器的磁盘使用情况
rolysh web[01-05] --command "df -h | grep /dev/sda1"

# 在两台主机上运行交互式 shell
rolysh host1,host2

# 通过 stdin 管道传递命令
echo "ls -la" | rolysh host1,host2

# 启用调试日志
rolysh host[1-10] --command "whoami" --debug
```

## 构建

```bash
cargo build
cargo build --release
```

## 测试

```bash
cargo test
```

## 许可证

MIT 许可证 - 详见 [LICENSE](LICENSE) 文件。