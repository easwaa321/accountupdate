use clap::Parser;
use futures::{SinkExt, StreamExt};
use pump_interface::accounts::PoolAccount;
use spl_token::state::{Account as SplAccount, Mint as SplMint};
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

    // Prompt for the four account pubkeys
    print!("Pool address: "); io::stdout().flush()?;
    let mut pool = String::new(); io::stdin().read_line(&mut pool)?;
    print!("Base vault address: "); io::stdout().flush()?;
    let mut base = String::new(); io::stdin().read_line(&mut base)?;
    print!("Quote vault address: "); io::stdout().flush()?;
    let mut quote = String::new(); io::stdin().read_line(&mut quote)?;
    print!("Base-token mint address: "); io::stdout().flush()?;
    let mut base_mint = String::new(); io::stdin().read_line(&mut base_mint)?;

    let pool = pool.trim().to_string();
    let base = base.trim().to_string();
    let quote = quote.trim().to_string();
    let base_mint = base_mint.trim().to_string();

    // Connect to the Geyser gRPC endpoint
    let mut client = GeyserGrpcClient::build_from_shared(args.endpoint)?
        .x_token(Some(args.x_token))?
        .tls_config(ClientTlsConfig::new().with_native_roots())?
        .connect()
        .await?;

    // Build a single subscription filtering for pool, vaults, and mint
    let filter = SubscribeRequestFilterAccounts {
        account: vec![pool.clone(), base.clone(), quote.clone(), base_mint.clone()],
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

    // Store mint decimals once unpacked
    let mut mint_decimals: Option<u8> = None;

    // Handle incoming updates
    while let Some(message) = stream.next().await {
        let message = message?;
        match message.update_oneof {
            Some(UpdateOneof::Account(acc)) => {
                if let Some(data) = acc.account {
                    let pk = bs58::encode(&data.pubkey).into_string();
                    // If this is the mint account, unpack decimals
                    if pk == base_mint {
                        match SplMint::unpack(&data.data) {
                            Ok(mint_acc) => {
                                mint_decimals = Some(mint_acc.decimals);
                                println!("Mint {} decimals set to {}", pk, mint_acc.decimals);
                            }
                            Err(e) => eprintln!("Failed to unpack mint {}: {}", pk, e),
                        }
                        continue;
                    }
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
                                if let Some(dec) = mint_decimals {
                                    let human = (raw as f64) / 10f64.powi(dec as i32);
                                    println!(
                                        "Base vault {} @ slot {}: {:.6} tokens (raw={})",
                                        pk, acc.slot, human, raw
                                    );
                                } else {
                                    println!(
                                        "Base vault {} @ slot {} raw tokens={} (mint decimals unknown)",
                                        pk, acc.slot, raw
                                    );
                                }
                            }
                            Err(e) => eprintln!("Failed to unpack base vault {}: {}", pk, e),
                        }
                    }
                    // Quote vault -> SOL balance
                    else if pk == quote {
                        let sol = (data.lamports as f64) / 1_000_000_000.0;
                        println!(
                            "Quote vault {} @ slot {} ~{:.9} SOL (lamports={})",
                            pk, acc.slot, sol, data.lamports
                        );
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
    Ok(())
}