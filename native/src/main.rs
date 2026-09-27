mod config;
mod error;
mod protocol;
mod server;
mod storage;

#[tokio::main]
async fn main() {
    if let Err(error) = server::run().await {
        eprintln!("[adapter] {error}");
        std::process::exit(1);
    }
}
