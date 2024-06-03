use std::collections::HashMap;
use std::pin::pin;
use std::str::FromStr;
use std::sync::Arc;
use std::thread::sleep;
use std::time::Duration;
use futures_util::FutureExt;
use geyser_grpc_connector::{GrpcConnectionTimeouts, GrpcSourceConfig, Message};
use geyser_grpc_connector::grpc_subscription_autoreconnect_streams::create_geyser_reconnecting_stream;
use serde_json::json;
use solana_account_decoder::parse_token::spl_token_ids;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client::rpc_client::GetConfirmedSignaturesForAddress2Config;
use solana_rpc_client_api::request::TokenAccountsFilter;
use solana_rpc_client_api::response::SlotInfo;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::pubkey::Pubkey;
use tokio::{join, select};
use tokio::sync::mpsc::error::SendError;
use tokio::sync::mpsc::Sender;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;
use tokio_stream::{Stream, StreamExt};
use tokio_stream::wrappers::{BroadcastStream, ReceiverStream};
use tracing::{info, trace, warn};
use websocket_tungstenite_retry::websocket_stable::{StableWebSocket, WsMessage};
use url::Url;
use yellowstone_grpc_proto::geyser::{SubscribeRequest, SubscribeRequestFilterAccounts, SubscribeRequestFilterSlots, SubscribeUpdate};
use yellowstone_grpc_proto::geyser::subscribe_update::UpdateOneof;

type Slot = u64;

enum Check {
    Gpa
}

#[tokio::main(flavor = "multi_thread", worker_threads = 16)]
async fn main() {
    tracing_subscriber::fmt::init();

    let ws_url2 = format!("wss://mango.rpcpool.com/{MAINNET_API_TOKEN}",
                          MAINNET_API_TOKEN = std::env::var("MAINNET_API_TOKEN").unwrap());
    let rpc_url = format!("https://mango.rpcpool.com/{MAINNET_API_TOKEN}",
                          MAINNET_API_TOKEN = std::env::var("MAINNET_API_TOKEN").unwrap());
    let rpc_url = Url::parse(rpc_url.as_str()).unwrap();
    let rpc_client = Arc::new(RpcClient::new(rpc_url.to_string()));

    let grpc_addr = std::env::var("GRPC_ADDR").unwrap();

    let geyser_grpc_timeouts = GrpcConnectionTimeouts {
        connect_timeout: Duration::from_secs(10),
        request_timeout: Duration::from_secs(10),
        subscribe_timeout: Duration::from_secs(10),
        receive_timeout: Duration::from_secs(10),
    };

    let geyser_grpc_config = GrpcSourceConfig::new(grpc_addr.to_string(), None, None, geyser_grpc_timeouts.clone());


    let mut all_check_tasks: JoinSet<Check> = JoinSet::new();


    all_check_tasks.spawn(rpc_gpa(rpc_client.clone()).then(|x| async { Check::Gpa }));


    all_check_tasks.spawn(rpc_get_token_accounts_by_owner(rpc_client.clone()).then(|x| async { Check::Gpa }));

    all_check_tasks.spawn(rpc_get_signatures_for_address(rpc_client.clone()).then(|x| async { Check::Gpa }));


    all_check_tasks.spawn(rpc_get_account_info(rpc_client.clone()).then(|x| async { Check::Gpa }));
    all_check_tasks.spawn(create_geyser_all_accounts_task(geyser_grpc_config.clone()).then(|x| async { Check::Gpa }));

    all_check_tasks.spawn(create_geyser_token_account_task(geyser_grpc_config.clone()).then(|x| async { Check::Gpa }));

    all_check_tasks.spawn(websocket_account_subscribe(Url::parse(ws_url2.as_str()).unwrap()).then(|x| async { Check::Gpa }));

    info!("all tasks started...");

    while let Some(res) = all_check_tasks.join_next().await {
        info!("one more Task completed: {} left", all_check_tasks.len());
    }

    info!("all tasks completed...");

}


// note: this might fail if the yellowstone plugin does not allow "any broadcast filter"
async fn create_geyser_all_accounts_task(config: GrpcSourceConfig) {
    let green_stream = create_geyser_reconnecting_stream(
        config.clone(),
        all_accounts(),
    );

    let mut count = 0;
    let mut green_stream = pin!(green_stream);
    while let Some(message) = green_stream.next().await {
        match message {
            Message::GeyserSubscribeUpdate(subscriber_update) => {
                match subscriber_update.update_oneof {
                    Some(UpdateOneof::Account(update)) => {
                        info!("Account from geyser: {:?}", update.account.unwrap().pubkey);
                        count += 1;
                        if count > 3 {
                            return;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    };
}

async fn create_geyser_token_account_task(config: GrpcSourceConfig) {
    let green_stream = create_geyser_reconnecting_stream(
        config.clone(),
        token_accounts(),
    );

    let mut count = 0;
    let mut green_stream = pin!(green_stream);
    while let Some(message) = green_stream.next().await {
        match message {
            Message::GeyserSubscribeUpdate(subscriber_update) => {
                match subscriber_update.update_oneof {
                    Some(UpdateOneof::Account(update)) => {
                        info!("Token Account: {:?}", update.account.unwrap().pubkey);
                        count += 1;
                        if count > 3 {
                            return;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    };
}

async fn rpc_gpa(rpc_client: Arc<RpcClient>)  {

    // TODO choose a smaller program
    // 4MangoMjqJ2firMokCjjGgoK8d4MXcrgL7XJaL3w6fVg
    let program_pubkey = Pubkey::from_str("CPLT8dWFQ1VH4ZJkvqSrLLFFPtCcKDm4XJ51t4K4mEiN").unwrap();

    // tokio::time::sleep(Duration::from_millis(100)).await;
    let program_accounts = rpc_client
        .get_program_accounts(&program_pubkey)
        .await
        .unwrap();

    info!("Program accounts: {:?}", program_accounts.len());
    // mango 12400 on mainnet
    // CPL: 107 on mainnet

}

async fn rpc_get_account_info(rpc_client: Arc<RpcClient>) {
    let program_pubkey = Pubkey::from_str("4MangoMjqJ2firMokCjjGgoK8d4MXcrgL7XJaL3w6fVg").unwrap();

    let account_info = rpc_client
        .get_account(&program_pubkey)
        .await
        .unwrap();

    info!("Account info: {:?}", account_info);

}

async fn rpc_get_token_accounts_by_owner(rpc_client: Arc<RpcClient>) {
    let owner_pubkey = Pubkey::from_str("SCbotdTZN5Vu9h4PgSAFoJozrALn2t5qMVdjyBuqu2c").unwrap();
    let mint = Pubkey::from_str("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").unwrap();

    let token_accounts = rpc_client
        .get_token_accounts_by_owner(
            &owner_pubkey,
            TokenAccountsFilter::Mint(mint),
        )
        .await
        .unwrap();

    // 1 account
    info!("Token accounts: {:?}", token_accounts.len());
}

async fn rpc_get_signatures_for_address(rpc_client: Arc<RpcClient>) {
    let address = Pubkey::from_str("SCbotdTZN5Vu9h4PgSAFoJozrALn2t5qMVdjyBuqu2c").unwrap();

    let config = GetConfirmedSignaturesForAddress2Config {
        before: None,
        until: None,
        limit: Some(42),
        commitment: Some(CommitmentConfig::confirmed()),
    };

    let signatures = rpc_client
        .get_signatures_for_address_with_config(&address, config)
        .await
        .unwrap();

    // 42
    info!("Signatures: {:?}", signatures.len());
}


async fn websocket_account_subscribe(
    rpc_url: Url
)  {

    let sysvar_subscribe =
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "accountSubscribe",
            "params": [
                "SysvarC1ock11111111111111111111111111111111"
            ]
        });

    let mut ws1 = StableWebSocket::new_with_timeout(
        rpc_url,
        sysvar_subscribe.clone(),
        Duration::from_secs(3),
    )
        .await
        .unwrap();

    let mut channel = ws1.subscribe_message_channel();

    let mut count = 0;
    while let Ok(msg) = channel.recv().await {
        if let WsMessage::Text(payload) = msg {
            info!("SysvarC1ock: {:?}", payload);
            count += 1;
        }
        if count > 3 {
            return;
        }
    }
}



pub fn all_accounts() -> SubscribeRequest {
    let mut accounts_subs = HashMap::new();
    accounts_subs.insert(
        "client".to_string(),
        SubscribeRequestFilterAccounts {
            account: vec![],
            owner: vec![],
            filters: vec![],
        },
    );

    SubscribeRequest {
        slots: Default::default(),
        accounts: accounts_subs,
        transactions: HashMap::new(),
        entry: Default::default(),
        blocks: Default::default(),
        blocks_meta: HashMap::new(),
        commitment: None,
        accounts_data_slice: Default::default(),
        ping: None,
    }
}



pub fn token_accounts() -> SubscribeRequest {
    let mut accounts_subs = HashMap::new();
    accounts_subs.insert(
        "client".to_string(),
        SubscribeRequestFilterAccounts {
            account: vec![],
            // vec!["4DoNfFBfF7UokCC2FQzriy7yHK6DY6NVdYpuekQ5pRgg".to_string()],
            owner:
            spl_token_ids().iter().map(|pubkey| pubkey.to_string()).collect(),
            filters: vec![],
        },
    );


    SubscribeRequest {
        slots: HashMap::new(),
        accounts: accounts_subs,
        transactions: HashMap::new(),
        entry: Default::default(),
        blocks: Default::default(),
        blocks_meta: HashMap::new(),
        commitment: None,
        accounts_data_slice: Default::default(),
        ping: None,
    }
}
