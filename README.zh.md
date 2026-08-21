# Rolysh

[English](README.md) | **中文**

一个基于 Rust 的现代化并行 SSH 连接工具。使用直观的主机语法和高效的异步 I/O，同时在多台主机上执行命令。

> **注意**：本项目是 [polysh](https://github.com/innogames/polysh) 的 Rust 重写版本，旨在解决原 Python 实现中的异步 I/O 和兼容性问题。

## 快速上手

```bash
# 连接几台主机（每个主机是独立的命令行参数）
rolysh host1 host2

# 连接列表文件中的所有主机
rolysh --hosts ip.list
```

## 功能特性

- **并行 SSH 执行**：同时在多台主机上运行命令或 shell
- **主机范围扩展**：`host<01-10>` 扩展为 `host01`、`host02`、...、`host10`
- **交互模式**：未提供命令时进入带历史记录的行编辑器 shell
- **非交互模式**：通过 stdin 管道传递命令或使用 `--command` 标志
- **自定义 SSH 命令**：通过 `--ssh` 接入任意 SSH 客户端
- **远程 shell 自动检测**：自动识别 bash/zsh/fish，也可用 `--force-shell` 强制指定
- **高效异步 I/O**：基于 Tokio 构建，无轮询
- **文件描述符限制自动调整**：大量主机时自动提升 ulimit
- **可配置日志**：`--debug` / `RUST_LOG`，支持 `--log-file` 输出到文件
- **彩色输出**：每行输出带有彩色主机名前缀，可用 `--no-color` 关闭

## 环境要求

- **nightly** 版 Rust 工具链（crate 使用了 `#![feature(bstr)]`；仓库中附带锁定 nightly 的 `rust-toolchain.toml`）
- 本地系统上有 `ssh` 客户端（或通过 `--ssh` 指定替代命令）

## 安装

### 从源码安装

```bash
git clone https://github.com/yourusername/rolysh.git
cd rolysh
cargo +nightly build --release
sudo cp target/release/rolysh /usr/local/bin/
```

### 通过 Cargo 安装

```bash
cargo +nightly install --git https://github.com/yourusername/rolysh.git
```

## 使用方法

```
rolysh [OPTIONS] HOSTS...
```

### 快速示例

```bash
# 在主机范围上执行命令后退出
rolysh web<01-05> -c "df -h"

# 在任意主机上执行命令（每个主机是独立的命令行参数）
rolysh host1 host2 db.prod --command "uptime"

# 交互模式：不带 -c 且 stdin 为终端 -> 共享 shell
rolysh web<01-05>

# 通过 stdin 管道传递命令
echo "ls -la" | rolysh host1 host2

# 以其他用户连接，连接出错时立即退出
rolysh -u deploy --abort-errors web<01-10> -c "systemctl status nginx"

# 从文件读取主机名（# 注释会被跳过）
rolysh --hosts-file hosts.txt -c "uname -a"

# 带调试日志输出到文件
rolysh host1 host2 -c "whoami" --debug --log-file /tmp/rolysh.log
```

### 主机语法

- 每个主机都是命令行上的**独立参数**（不支持逗号列表）：
  `rolysh host1 host2 host3`
- 范围扩展：`web<01-10>` 扩展为 `web01` ... `web10`。
  前导零位数取自范围的起始值：`host<1-5>` → `host1`..`host5`，
  `host<01-5>` → `host01`..`host05`。
- 显式指定端口：`host:2222`。IPv6 地址使用中括号：`[fe80::1]:2222`。
- 同一主机出现多次时，输出中会显示为 `host#1`、`host#2` 等以作区分。

### 选项

```
--version                      显示版本
-h, --help                     显示帮助信息
--hosts-file|--hosts FILE      从文件读取主机名（支持 # 注释）
--command|-c CMD               执行命令后退出（非交互模式）
--ssh SSH                      使用的 SSH 命令
                               （默认：exec ssh -oLogLevel=Quiet -t %(host)s %(port)s）
--user|-u USER                 远程登录用户
--no-color                     关闭彩色主机名
--password-file FILE           从文件读取密码（- 表示从 stdin 读取）
--password|-p 'PASSWD'         从命令行设置密码
--log-file FILE                将所有 I/O 记录到文件（配合 --debug 使用）
--separator|--sep SEP          输出行主机名后面的分隔符（默认：:）
--abort-errors                 连接出错时退出
--debug                        启用调试输出
--force-shell SHELL            强制指定远程 shell 类型：bash, zsh, fish, auto
```

`--ssh` 命令模板支持 `%(host)s` 和 `%(port)s` 替换，例如：

```bash
rolysh --ssh "ssh -i ~/.ssh/deploy_ed25519 -t %(host)s %(port)s" host1 -c "id"
```

### 交互模式控制命令

交互模式下，以 `:` 开头的行是本地控制命令；以 `!` 开头的行在**本地** shell 中执行（`sh -c`）：

```
:list, :l             列出所有远程连接及其状态
:enable [hosts...]    启用主机（不带参数则启用全部）
:disable [hosts...]   禁用主机（不带参数则禁用全部）
:quit, :q, :exit      退出会话
:help, :h             显示控制命令帮助
!ls -la               在本地执行 `ls -la`，而不是发送到远程主机
```

其他任意行都会发送到所有已启用的远程 shell。提示符会显示还有多少主机在等待输入，
例如 `wait (2/3)> ` 或 `ready (3)> `。
命令历史保存在 `~/.rolysh_history`。

### 批量模式

使用 `--command`（或管道 stdin）时，rolysh 会在所有主机上执行命令，
输出合并后的结果后退出。退出码为所有远程命令退出码中的最大值。
注意：`--command` 与从 stdin 读取命令二者互斥。

### 日志

- 默认日志以 `INFO` 级别输出到 stdout。
- `--debug` 将级别提升至 `DEBUG`，并在未指定 `--log-file` 时写入 `/tmp/rolysh.log`。
- 可通过 `RUST_LOG` 环境变量覆盖日志级别，例如 `RUST_LOG=rolysh=trace rolysh ...`。

## 构建

```bash
cargo +nightly build
cargo +nightly build --release
```

## 测试

```bash
cargo +nightly test
```

## 许可证

MIT 许可证 - 详见 [LICENSE](LICENSE) 文件。
