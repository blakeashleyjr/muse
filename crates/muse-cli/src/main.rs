use clap::Parser;
use muse_cli::{dispatch, Cli};
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let outcome = dispatch(cli).await;
    print!("{}", outcome.stdout);
    if outcome.success {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
