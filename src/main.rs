use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    meta_mcp::cli::run().await
}
