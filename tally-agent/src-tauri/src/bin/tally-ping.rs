//! Standalone Tally ping utility — Build Order Step 1.
//! Connects to local Tally, logs company info and ALTERID to stdout.
//! Usage: cargo run --bin tally-ping [--port 9000]

use fininsight_tally_agent_lib::tally_client::{TallyClient, TallyEndpoint};

#[tokio::main]
async fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(9000);

    let endpoint = match TallyEndpoint::new("127.0.0.1", port) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Refusing to connect: {e}");
            std::process::exit(1);
        }
    };

    let client = match TallyClient::new(endpoint) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Client init failed: {e}");
            std::process::exit(1);
        }
    };

    match client.ping().await {
        Ok(info) => {
            println!("Company: {}", info.company_name);
            println!("ALTERID: {}", info.alter_id);
        }
        Err(e) => {
            eprintln!("Tally ping failed: {e}");
            std::process::exit(1);
        }
    }
}
