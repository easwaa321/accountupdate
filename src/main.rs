use clap::Parser;
use futures::{SinkExt, StreamExt};
use pump_interface::accounts::PoolAccount;
use spl_token::state::Account as SplAccount;
use solana_program::program_pack::Pack;
use std::io::{self, Write};
use tonic::transport::channel::ClientTlsConfig;
use yellowstone_grpc_client::GeyserGrpcClient;
use yellowstone_grpc_proto::prelude::{
    CommitmentLevel,
    SubscribeRequest,
    subscribe_update::UpdateOneof,
    SubscribeRequestFilterAccounts,
    SubscribeRequestPing,
};

#[derive(Debug)]
struct VaultUpdate {
    slot: u64,
    amount: f64,
}

#[derive(Debug)]
struct RamDb {
    pool_address: String,
    base_vault_address: String,
    quote_vault_address: String,
    base_updates: Vec<VaultUpdate>,
    quote_updates: Vec<VaultUpdate>,
}

#[derive(Parser)]
struct Args {
    /// Geyser gRPC endpoint URL
    #[clap(short, long)]
    endpoint: String,
    /// X-Token for authentication
    #[clap(long)]
    x_token: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Parse CLI arguments
    let args = Args::parse();

    // Prompt for account pubkeys
    let mut pool = String::new();
    let mut quote = String::new();
    let mut base = String::new();
    let mut base_mint = String::new();
    let mut quote_mint = String::new();

    print!("Pool address: ");
    io::stdout().flush()?;
    io::stdin().read_line(&mut pool)?;
    print!("Base vault address: ");
    io::stdout().flush()?;
    io::stdin().read_line(&mut base)?;
    print!("Base-token mint address: ");
    io::stdout().flush()?;
    io::stdin().read_line(&mut base_mint)?;
    print!("Quote vault address: ");
    io::stdout().flush()?;
    io::stdin().read_line(&mut quote)?;
    print!("Quote-token mint address: ");
    io::stdout().flush()?;
    io::stdin().read_line(&mut quote_mint)?;

    let pool = pool.trim().to_string();
    let base = base.trim().to_string();
    let quote = quote.trim().to_string();
    let base_mint = base_mint.trim().to_string();
    let quote_mint = quote_mint.trim().to_string();

    // Initialize in-memory database with static fields
    let mut ram_db = RamDb {
        pool_address: pool.clone(),
        base_vault_address: base.clone(),
        quote_vault_address: quote.clone(),
        base_updates: Vec::new(),
        quote_updates: Vec::new(),
    };

    // Connect to the Geyser gRPC endpoint
    let mut client = GeyserGrpcClient::build_from_shared(args.endpoint)?
        .x_token(Some(args.x_token))?
        .tls_config(ClientTlsConfig::new().with_native_roots())?
        .connect()
        .await?;

    // Build subscription filter for the desired accounts
    let accounts = vec![pool.clone(), base.clone(), quote.clone()];

    let filter = SubscribeRequestFilterAccounts {
        account: accounts,
        owner: Vec::new(),
        nonempty_txn_signature: None,
        filters: Vec::new(),
    };

    let request = SubscribeRequest {
        accounts: std::iter::once(("watched".into(), filter)).collect(),
        slots: Default::default(),
        transactions: Default::default(),
        transactions_status: Default::default(),
        blocks: Default::default(),
        blocks_meta: Default::default(),
        entry: Default::default(),
        commitment: Some(CommitmentLevel::Processed as i32),
        accounts_data_slice: Vec::new(),
        ping: None,
        from_slot: None,
    };

    // Subscribe once and start the stream
    let (mut tx, mut stream) = client.subscribe_with_request(Some(request)).await?;
    println!("Subscribed - receiving updates. Ctrl+C to exit.");

    // Determine token decimals based on mint addresses
    let base_decimals = if base_mint == "So11111111111111111111111111111111111111112" {
        9
    } else {
        6
    };
    let quote_decimals = if quote_mint == "So11111111111111111111111111111111111111112" {
        9
    } else {
        6
    };

    // Handle incoming updates
    while let Some(message) = stream.next().await {
        let message = message?;
        match message.update_oneof {
            Some(UpdateOneof::Account(acc)) => {
                if let Some(data) = acc.account {
                    let pk = bs58::encode(&data.pubkey).into_string();
                    // Pool PDA -> print lp_supply
                    if pk == pool {
                        let pool_state = PoolAccount::deserialize(&data.data)
                            .map_err(|e| anyhow::anyhow!("Failed to deserialize PoolAccount: {}", e))?;
                        let info = pool_state.0;
                        println!(
                            "Pool PDA {} @ slot {}: base_mint={} quote_mint={} lp_supply={}",
                            pk, acc.slot, info.base_mint, info.quote_mint, info.lp_supply
                        );
                    }
                    // Base vault -> token balance
                    else if pk == base {
                        match SplAccount::unpack(&data.data) {
                            Ok(token_acc) => {
                                let raw = token_acc.amount;
                                let human = (raw as f64) / 10f64.powi(base_decimals as i32);
                                println!(
                                    "Base vault {} @ slot {}: {:.6} tokens (raw={})",
                                    pk, acc.slot, human, raw
                                );
                                ram_db.base_updates.push(VaultUpdate { slot: acc.slot, amount: human });
                            }
                            Err(e) => eprintln!("Failed to unpack base vault {}: {}", pk, e),
                        }
                    }
                    // Quote vault -> SOL balance
                    else if pk == quote {
                        match SplAccount::unpack(&data.data) {
                            Ok(token_acc) => {
                                let raw = token_acc.amount;
                                let human = (raw as f64) / 10f64.powi(quote_decimals as i32);
                                println!(
                                    "Quote vault {} @ slot {}: {:.6} tokens (raw={})",
                                    pk, acc.slot, human, raw
                                );
                                ram_db.quote_updates.push(VaultUpdate { slot: acc.slot, amount: human });
                            }
                            Err(e) => eprintln!("Failed to unpack quote vault {}: {}", pk, e),
                        }
                    }
                }
            }
            Some(UpdateOneof::Ping(_)) => {
                // Respond to heartbeat
                tx.send(SubscribeRequest {
                    ping: Some(SubscribeRequestPing { id: 1 }),
                    ..Default::default()
                })
                .await
                .ok();
            }
            _ => {}
        }
    }

    println!("Final RAM DB: {:?}", ram_db);
    Ok(())
}