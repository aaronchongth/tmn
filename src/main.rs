use std::process::ExitCode;

fn main() -> ExitCode {
    match tmn::cli::parse_args(std::env::args_os()) {
        Ok(tmn::cli::CliOutcome::Run(_cli)) => ExitCode::SUCCESS,
        Ok(tmn::cli::CliOutcome::Printed(s)) => {
            print!("{s}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            if e.message.ends_with('\n') {
                eprint!("{}", e.message);
            } else {
                eprintln!("{}", e.message);
            }
            ExitCode::from(e.exit_code as u8)
        }
    }
}
