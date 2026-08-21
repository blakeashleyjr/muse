use clap::Parser;
use muse_cli::{dispatch, Cli};
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    muse_cli::init_logging();
    let cli = Cli::parse();
    let outcome = dispatch(cli).await;
    print!("{}", outcome.stdout);
    ExitCode::from(outcome.code)
}
