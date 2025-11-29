use crate::config::Config;
use crate::errors::{Error, Result};
use std::env;
use std::io::{self, Read};
use std::path::PathBuf;

pub fn parse_args() -> Result<Config> {
    let args: Vec<String> = env::args().collect();
    let mut config = Config::default();
    let mut i = 1;

    while i < args.len() {
        match args[i].as_str() {
            "--version" => {
                println!("rolysh 0.1.0");
                std::process::exit(0);
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            "--hosts-file" => {
                i += 1;
                if i >= args.len() {
                    return Err(Error::InvalidArgs("--hosts-file requires an argument".into()));
                }
                let hosts = Config::load_hosts_file(&args[i])
                    .map_err(|e| Error::InvalidArgs(format!("Failed to read hosts file: {e}")))?;
                config.host_names.extend(hosts);
            }
            "--command" => {
                i += 1;
                if i >= args.len() {
                    return Err(Error::InvalidArgs("--command requires an argument".into()));
                }
                config.command = Some(args[i].clone());
            }
            "--ssh" => {
                i += 1;
                if i >= args.len() {
                    return Err(Error::InvalidArgs("--ssh requires an argument".into()));
                }
                config.ssh_cmd = args[i].clone();
            }
            "--user" => {
                i += 1;
                if i >= args.len() {
                    return Err(Error::InvalidArgs("--user requires an argument".into()));
                }
                config.user = Some(args[i].clone());
            }
            "--no-color" => {
                config.disable_color = true;
            }
            "--password-file" => {
                i += 1;
                if i >= args.len() {
                    return Err(Error::InvalidArgs("--password-file requires an argument".into()));
                }
                let password_file = &args[i];
                config.password = Some(read_password(password_file)?);
            }
            "--log-file" => {
                i += 1;
                if i >= args.len() {
                    return Err(Error::InvalidArgs("--log-file requires an argument".into()));
                }
                config.log_file = Some(PathBuf::from(&args[i]));
            }
            "--abort-errors" => {
                config.abort_on_error = true;
            }
            "--debug" => {
                config.debug = true;
            }
            arg if arg.starts_with("--") => {
                return Err(Error::InvalidArgs(format!("Unknown option: {arg}")));
            }
            _ => {
                // This is a hostname
                config.host_names.push(args[i].clone());
            }
        }
        i += 1;
    }

    // Handle reading from stdin if no command was provided
    if config.command.is_none() && !atty::is(atty::Stream::Stdin) {
        let mut stdin_input = String::new();
        io::stdin().read_to_string(&mut stdin_input)?;
        if !stdin_input.is_empty() {
            config.command = Some(stdin_input);
        }
    }

    // Determine if interactive mode
    config.interactive =
        config.command.is_none() && atty::is(atty::Stream::Stdin) && atty::is(atty::Stream::Stdout);

    if config.host_names.is_empty() {
        return Err(Error::InvalidArgs("No hosts specified".into()));
    }

    Ok(config)
}

fn read_password(file_path: &str) -> Result<String> {
    if file_path == "-" {
        // Read from terminal
        use std::io::Write;
        print!("Password: ");
        io::stdout().flush().ok();

        let mut password = String::new();
        io::stdin().read_line(&mut password)?;
        Ok(password.trim_end().to_string())
    } else {
        // Read from file
        std::fs::read_to_string(file_path)
            .map(|s| s.trim_end().to_string())
            .map_err(Error::Io)
    }
}

fn print_help() {
    println!(
        r#"rolysh - Control many SSH sessions at once

USAGE:
    rolysh [OPTIONS] HOSTS...

OPTIONS:
    --version              Show version
    -h, --help            Show this help message
    --hosts-file FILE     Read hostnames from a file
    --command CMD         Execute command and exit (non-interactive)
    --ssh SSH             SSH command to use
    --user USER           Remote user to log in as
    --no-color            Disable colored hostnames
    --password-file FILE  Read password from file (- for stdin)
    --log-file FILE       Log all I/O to a file
    --abort-errors        Exit on connection errors
    --debug               Enable debug output

CONTROL COMMANDS (prefixed with ':'):
    :add NAMES...         Add remote shells
    :list [SHELLS...]     List shells and their state
    :disable [SHELLS...]  Disable shells
    :enable [SHELLS...]   Enable shells
    :quit                 Exit rolysh

"#
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_hosts() {
        let args = vec!["rolysh".to_string(), "host1".to_string(), "host2".to_string()];
        env::set_var("ARGS_TEST", "true");
        // Note: This would need a proper test setup
    }
}
