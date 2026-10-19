use std::process::ExitCode;

fn main() -> ExitCode {
    match gitglass_log::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("gitglass-log: {error}");
            ExitCode::FAILURE
        }
    }
}
