use clap::Parser;
use std::ffi::OsString;

const EXAMPLES: &str = "\
Examples:
  tmn ros2 launch demo_nodes_cpp talker_listener_launch.py
  tmn ros2 launch pkg bringup.launch.py --ros-args -r __ns:=/bot1
  tmn --headless -n 50000 ros2 launch demo_nodes_cpp talker_listener_launch.py";

#[derive(Parser, Debug, Clone, PartialEq, Eq)]
#[command(
    name = "tmn",
    about = "Terminal UI log viewer for ros2 launch and multi-process commands",
    override_usage = "tmn [OPTIONS] [--] <COMMAND> [ARGS]...",
    after_help = EXAMPLES,
    version,
    max_term_width = 80
)]
pub struct Cli {
    /// Maximum log entries retained in the ring buffer (COUNT >= 1).
    #[arg(
        short = 'n',
        long,
        default_value = "100000",
        value_name = "COUNT",
        value_parser = parse_max_lines
    )]
    pub max_lines: usize,

    /// Do not inject default ROS/Python buffering and output env vars into the child.
    #[arg(long)]
    pub no_env_override: bool,

    /// Run command through PTY and parser without opening the TUI; print on exit.
    #[arg(long)]
    pub headless: bool,

    /// Command and arguments to run.
    #[arg(value_name = "COMMAND", trailing_var_arg = true)]
    pub command: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliOutcome {
    Run(Cli),
    Printed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliError {
    pub message: String,
    pub exit_code: i32,
}

fn parse_max_lines(val: &str) -> Result<usize, String> {
    let n: usize = val.parse().map_err(|_| format!("invalid integer: {val}"))?;
    if n == 0 {
        return Err("max-lines must be greater than 0".to_string());
    }
    Ok(n)
}

fn concise_help() -> String {
    "\
Terminal UI log viewer for ros2 launch and multi-process commands

Usage: tmn [OPTIONS] [--] <COMMAND> [ARGS]...

Examples:
  tmn ros2 launch demo_nodes_cpp talker_listener_launch.py
  tmn --headless ros2 launch pkg bringup.launch.py --ros-args -r __ns:=/bot1

Run 'tmn --help' for all options.
"
    .to_string()
}

pub fn parse_args<I, T>(args: I) -> Result<CliOutcome, CliError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    match Cli::try_parse_from(args) {
        Ok(cli) => {
            if cli.command.is_empty() {
                Err(CliError {
                    message: concise_help(),
                    exit_code: 2,
                })
            } else {
                Ok(CliOutcome::Run(cli))
            }
        }
        Err(err) => {
            if err.kind() == clap::error::ErrorKind::DisplayHelp
                || err.kind() == clap::error::ErrorKind::DisplayVersion
            {
                Ok(CliOutcome::Printed(err.render().to_string()))
            } else {
                Err(CliError {
                    message: err.render().to_string(),
                    exit_code: err.exit_code(),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_args_returns_concise_help_and_exit_2() {
        let err = parse_args(["tmn"]).unwrap_err();
        assert_eq!(err.exit_code, 2);
        assert!(err.message.contains("Usage: tmn"));
        assert!(err.message.contains("talker_listener_launch.py"));
        assert!(err.message.contains("tmn --help"));
    }

    #[test]
    fn trailing_ros_flags_pass_through_without_double_dash() {
        let CliOutcome::Run(cli) = parse_args([
            "tmn",
            "ros2",
            "launch",
            "pkg",
            "bringup.launch.py",
            "--ros-args",
            "-r",
            "__ns:=/bot1",
        ])
        .unwrap() else {
            panic!("expected Run")
        };
        assert_eq!(cli.max_lines, 100_000);
        assert!(!cli.no_env_override);
        assert!(!cli.headless);
        assert_eq!(
            cli.command,
            [
                "ros2",
                "launch",
                "pkg",
                "bringup.launch.py",
                "--ros-args",
                "-r",
                "__ns:=/bot1"
            ]
        );
    }

    #[test]
    fn rejects_zero_max_lines_and_unknown_flags() {
        assert_eq!(
            parse_args(["tmn", "--max-lines", "0", "echo"])
                .unwrap_err()
                .exit_code,
            2
        );
        assert_eq!(
            parse_args(["tmn", "--max-lines", "not-a-number", "echo"])
                .unwrap_err()
                .exit_code,
            2
        );
        let bad = parse_args(["tmn", "--healdess", "echo"]).unwrap_err();
        assert_eq!(bad.exit_code, 2);
        assert!(bad.message.contains("--headless"));
    }

    #[test]
    fn double_dash_separator_works() {
        let CliOutcome::Run(cli) = parse_args([
            "tmn",
            "--headless",
            "-n",
            "500",
            "--no-env-override",
            "--",
            "ros2",
            "launch",
            "my_pkg",
        ])
        .unwrap() else {
            panic!("expected Run")
        };
        assert_eq!(cli.max_lines, 500);
        assert!(cli.headless);
        assert!(cli.no_env_override);
        assert_eq!(cli.command, ["ros2", "launch", "my_pkg"]);
    }

    #[test]
    fn flags_without_command_returns_concise_help_and_exit_2() {
        let err = parse_args(["tmn", "--headless"]).unwrap_err();
        assert_eq!(err.exit_code, 2);
        assert!(err.message.contains("Usage: tmn"));
    }

    #[test]
    fn help_and_version_fit_80_cols_and_exit_0() {
        for flag in ["-h", "--help"] {
            let CliOutcome::Printed(help) = parse_args(["tmn", flag]).unwrap() else {
                panic!()
            };
            assert!(
                help.lines().all(|l| l.chars().count() <= 80),
                "line exceeded 80 cols in {flag}:\n{help}"
            );
            assert!(help.contains("Usage: tmn [OPTIONS] [--] <COMMAND> [ARGS]..."));
            assert!(help.contains("Examples:"));
            assert!(help.contains("talker_listener_launch.py"));
        }
        let CliOutcome::Printed(ver) = parse_args(["tmn", "-V"]).unwrap() else {
            panic!()
        };
        assert!(ver.starts_with("tmn "));
    }
}
