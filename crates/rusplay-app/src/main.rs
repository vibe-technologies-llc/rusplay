use std::process::ExitCode;

use rusplay_client::ClientSession;
use rusplay_config::Config;

fn main() -> ExitCode {
    match ClientSession::new(Config::default()).run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rusplay: {error}");
            ExitCode::FAILURE
        }
    }
}
