use std::collections::HashMap;
use std::pin::pin;
use std::str::FromStr;
use std::thread::sleep;
use std::time::Duration;
use geyser_grpc_connector::{GrpcConnectionTimeouts, GrpcSourceConfig, Message};
use geyser_grpc_connector::grpc_subscription_autoreconnect_streams::create_geyser_reconnecting_stream;
use serde_json::json;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client::rpc_client::GetConfirmedSignaturesForAddress2Config;
use solana_rpc_client_api::request::TokenAccountsFilter;
use solana_rpc_client_api::response::SlotInfo;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::pubkey::Pubkey;
use tokio::select;
use tokio::sync::mpsc::error::SendError;
use tokio::sync::mpsc::Sender;
use tokio::time::Instant;
use tokio_stream::{Stream, StreamExt};
use tokio_stream::wrappers::{BroadcastStream, ReceiverStream};
use tracing::{info, trace, warn};
use websocket_tungstenite_retry::websocket_stable::{StableWebSocket, WsMessage};
use url::Url;
use yellowstone_grpc_proto::geyser::{SubscribeRequest, SubscribeRequestFilterAccounts, SubscribeRequestFilterSlots, SubscribeUpdate};
use yellowstone_grpc_proto::geyser::subscribe_update::UpdateOneof;

type Slot = u64;

#[tokio::main(flavor = "multi_thread", worker_threads = 16)]
async fn main() {
    tracing_subscriber::fmt::init();

    let ws_url1 = format!("wss://api.mainnet-beta.solana.com");
    let ws_url2 = format!("wss://mango.rpcpool.com/{MAINNET_API_TOKEN}",
                          MAINNET_API_TOKEN = std::env::var("MAINNET_API_TOKEN").unwrap());
    let rpc_url = format!("https://mango.rpcpool.com/{MAINNET_API_TOKEN}",
                          MAINNET_API_TOKEN = std::env::var("MAINNET_API_TOKEN").unwrap());
    let rpc_url = Url::parse(rpc_url.as_str()).unwrap();
    let rpc_client = RpcClient::new(rpc_url.to_string());

    let grpc_addr = std::env::var("GRPC_ADDR").unwrap();

    rpc_gpa(&rpc_client).await;

    rpc_get_account_info(&rpc_client).await;

    rpc_get_token_accounts_by_owner(&rpc_client).await;

    rpc_get_signatures_for_address(&rpc_client).await;

    websocket_account_subscribe(Url::parse(ws_url2.as_str()).unwrap()).await;

    let (slots_tx, mut slots_rx) = tokio::sync::mpsc::channel(100);

    let timeouts = GrpcConnectionTimeouts {
        connect_timeout: Duration::from_secs(10),
        request_timeout: Duration::from_secs(10),
        subscribe_timeout: Duration::from_secs(10),
        receive_timeout: Duration::from_secs(10),
    };


    let config = GrpcSourceConfig::new(grpc_addr.to_string(), None, None, timeouts.clone());

    start_geyser_all_accounts_task(config.clone());

    start_geyser_orca_token_account_task(config.clone());

    tokio::spawn(websocket_source(Url::parse(ws_url1.as_str()).unwrap(), slots_tx.clone()));
    tokio::spawn(websocket_source(Url::parse(ws_url2.as_str()).unwrap(), slots_tx.clone()));
    tokio::spawn(rpc_getslot_source(rpc_url, slots_tx.clone()));


    let started_at = Instant::now();
    while let Some(slot) = slots_rx.recv().await {
        println!("Slot: {}", slot);

        if Instant::now().duration_since(started_at) > Duration::from_secs(2) {
            break;
        }
    }

   sleep(Duration::from_secs(15));
}


// note: this might fail if the yellowstone plugin does not allow "any broadcast filter"
fn start_geyser_all_accounts_task(config: GrpcSourceConfig) {
    let green_stream = create_geyser_reconnecting_stream(
        config.clone(),
        // orca_token_account(),
        all_accounts(),
    );

    tokio::spawn(async move {
        let mut green_stream = pin!(green_stream);
        while let Some(message) = green_stream.next().await {
            match message {
                Message::GeyserSubscribeUpdate(subscriber_update) => {
                    match subscriber_update.update_oneof {
                        Some(UpdateOneof::Account(update)) => {
                            info!("ORCA Account: {:?}", update.account.unwrap().pubkey);
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    });
}

fn start_geyser_orca_token_account_task(config: GrpcSourceConfig) {
    let green_stream = create_geyser_reconnecting_stream(
        config.clone(),
        orca_token_account(),
    );

    tokio::spawn(async move {
        let mut green_stream = pin!(green_stream);
        while let Some(message) = green_stream.next().await {
            match message {
                Message::GeyserSubscribeUpdate(subscriber_update) => {
                    match subscriber_update.update_oneof {
                        Some(UpdateOneof::Account(update)) => {
                            info!("ORCA Account: {:?}", update.account.unwrap().pubkey);
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    });
}

async fn rpc_gpa(rpc_client: &RpcClient)  {

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

async fn rpc_get_account_info(rpc_client: &RpcClient) {
    let program_pubkey = Pubkey::from_str("4MangoMjqJ2firMokCjjGgoK8d4MXcrgL7XJaL3w6fVg").unwrap();

    let account_info = rpc_client
        .get_account(&program_pubkey)
        .await
        .unwrap();

    info!("Account info: {:?}", account_info);

}

async fn rpc_get_token_accounts_by_owner(rpc_client: &RpcClient) {
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

async fn rpc_get_signatures_for_address(rpc_client: &RpcClient) {
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
            break;
        }
    }
}



async fn rpc_getslot_source(
    rpc_url: Url,
    mpsc_downstream: tokio::sync::mpsc::Sender<Slot>,
)  {

    let rpc = RpcClient::new(rpc_url.to_string());
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let slot = rpc
            .get_slot_with_commitment(CommitmentConfig::processed())
            .await
            .unwrap();
        match mpsc_downstream.send(slot).await {
            Ok(_) => {}
            Err(_) => return
        }
    }

}


async fn websocket_source(
    rpc_url: Url,
    mpsc_downstream: tokio::sync::mpsc::Sender<Slot>,
)  {

    let processed_slot_subscribe =
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "slotSubscribe",
        });

    let mut ws1 = StableWebSocket::new_with_timeout(
        rpc_url,
        processed_slot_subscribe.clone(),
        Duration::from_secs(3),
    )
        .await
        .unwrap();

    let mut channel = ws1.subscribe_message_channel();

    while let Ok(msg) = channel.recv().await {
        if let WsMessage::Text(payload) = msg {
            let ws_result: jsonrpsee_types::SubscriptionResponse<SlotInfo> = serde_json::from_str(&payload).unwrap();
            let slot_info = ws_result.params.result;
            match mpsc_downstream.send(slot_info.slot).await {
                Ok(_) => {}
                Err(_) => return
            }
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



pub fn orca_token_account() -> SubscribeRequest {
    let mut accounts_subs = HashMap::new();
    accounts_subs.insert(
        "client".to_string(),
        SubscribeRequestFilterAccounts {
            account: vec!["2WLWEuKDgkDUccTpbwYp1GToYktiSB1cXvreHUwiSUVP".to_string()],
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


