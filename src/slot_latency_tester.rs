use geyser_grpc_connector::grpc_subscription_autoreconnect_streams::create_geyser_reconnecting_stream;
use geyser_grpc_connector::{GrpcConnectionTimeouts, GrpcSourceConfig, Message};
use serde_json::json;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client::rpc_client::GetConfirmedSignaturesForAddress2Config;
use solana_rpc_client_api::request::TokenAccountsFilter;
use solana_rpc_client_api::response::SlotInfo;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::pin::pin;
use std::str::FromStr;
use std::thread::sleep;
use std::time::Duration;
use tokio::select;
use tokio::sync::mpsc::error::SendError;
use tokio::time::Instant;
use tokio_stream::StreamExt;
use tracing::info;
use url::Url;
use websocket_tungstenite_retry::websocket_stable::{StableWebSocket, WsMessage};
use yellowstone_grpc_proto::geyser::subscribe_update::UpdateOneof;
use yellowstone_grpc_proto::geyser::{
    SubscribeRequest, SubscribeRequestFilterAccounts, SubscribeRequestFilterSlots, SubscribeUpdate,
};

type Slot = u64;

#[derive(Debug, Clone)]
enum SlotSource {
    SolanaWebsocket,
    SolanaRpc,
    TritonRpc,
    TritonWebsocket,
    YellowstoneGrpc,
}

struct SlotDatapoint {
    source: SlotSource,
    slot: Slot,
    timestamp: Instant,
}

impl SlotDatapoint {
    fn new(source: SlotSource, slot: Slot) -> Self {
        Self {
            source,
            slot,
            timestamp: Instant::now(),
        }
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 16)]
async fn main() {
    tracing_subscriber::fmt::init();

    // TODO add solana rpc
    let solana_rpc_url = format!("https://api.mainnet-beta.solana.com");
    let solana_ws_url = format!("wss://api.mainnet-beta.solana.com");
    let triton_ws_url = format!(
        "wss://mango.rpcpool.com/{MAINNET_API_TOKEN}",
        MAINNET_API_TOKEN = std::env::var("MAINNET_API_TOKEN").unwrap()
    );
    let triton_rpc_url = format!(
        "https://mango.rpcpool.com/{MAINNET_API_TOKEN}",
        MAINNET_API_TOKEN = std::env::var("MAINNET_API_TOKEN").unwrap()
    );
    let solana_rpc_url = Url::parse(solana_rpc_url.as_str()).unwrap();
    let triton_rpc_url = Url::parse(triton_rpc_url.as_str()).unwrap();



    let grpc_addr = std::env::var("GRPC_ADDR").unwrap();

    let timeouts = GrpcConnectionTimeouts {
        connect_timeout: Duration::from_secs(10),
        request_timeout: Duration::from_secs(10),
        subscribe_timeout: Duration::from_secs(10),
        receive_timeout: Duration::from_secs(10),
    };

    let config = GrpcSourceConfig::new(grpc_addr.to_string(), None, None, timeouts.clone());

    let (slots_tx, mut slots_rx) = tokio::sync::mpsc::channel::<SlotDatapoint>(100);

    start_geyser_slots_task(config.clone(), SlotSource::YellowstoneGrpc, slots_tx.clone());

    tokio::spawn(websocket_source(
        Url::parse(solana_ws_url.as_str()).unwrap(),
        SlotSource::SolanaWebsocket,
        slots_tx.clone(),
    ));
    tokio::spawn(websocket_source(
        Url::parse(triton_ws_url.as_str()).unwrap(),
        SlotSource::TritonWebsocket,
        slots_tx.clone(),
    ));
    tokio::spawn(rpc_getslot_source(solana_rpc_url, SlotSource::SolanaRpc, slots_tx.clone()));
    tokio::spawn(rpc_getslot_source(triton_rpc_url, SlotSource::TritonRpc, slots_tx.clone()));

    let started_at = Instant::now();
    while let Some(SlotDatapoint { slot, source, .. }) = slots_rx.recv().await {
        println!("Slot from {:?}: {}", source, slot);

        if Instant::now().duration_since(started_at) > Duration::from_secs(2) {
            break;
        }
    }

    sleep(Duration::from_secs(15));
}

async fn rpc_getslot_source(rpc_url: Url, slot_source: SlotSource, mpsc_downstream: tokio::sync::mpsc::Sender<SlotDatapoint>) {
    let rpc = RpcClient::new(rpc_url.to_string());
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let slot = rpc
            .get_slot_with_commitment(CommitmentConfig::processed())
            .await
            .unwrap();
        match mpsc_downstream.send(SlotDatapoint::new(slot_source.clone(), slot)).await {
            Ok(_) => {}
            Err(_) => return,
        }
    }
}

async fn websocket_source(rpc_url: Url, slot_source: SlotSource,
                          mpsc_downstream: tokio::sync::mpsc::Sender<SlotDatapoint>) {
    let processed_slot_subscribe = json!({
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
            let ws_result: jsonrpsee_types::SubscriptionResponse<SlotInfo> =
                serde_json::from_str(&payload).unwrap();
            let slot_info = ws_result.params.result;
            match mpsc_downstream.send(SlotDatapoint::new(slot_source.clone(), slot_info.slot)).await {
                Ok(_) => {}
                Err(_) => return,
            }
        }
    }
}

// note: this might fail if the yellowstone plugin does not allow "any broadcast filter"
fn start_geyser_slots_task(
    config: GrpcSourceConfig,
    slot_source: SlotSource,
    mpsc_downstream: tokio::sync::mpsc::Sender<SlotDatapoint>,
) {
    let green_stream = create_geyser_reconnecting_stream(config.clone(), slots());

    tokio::spawn(async move {
        let mut green_stream = pin!(green_stream);
        while let Some(message) = green_stream.next().await {
            if let Message::GeyserSubscribeUpdate(subscriber_update) = message {
                if let Some(UpdateOneof::Slot(slot_info)) = subscriber_update.update_oneof {
                    match mpsc_downstream.send(SlotDatapoint::new(slot_source.clone(), slot_info.slot)).await {
                        Ok(_) => {}
                        Err(_) => return,
                    }
                }
            }
        }
    });
}

pub fn slots() -> SubscribeRequest {
    let mut slot_subs = HashMap::new();
    slot_subs.insert(
        "client".to_string(),
        SubscribeRequestFilterSlots {
            filter_by_commitment: Some(true),
        },
    );

    SubscribeRequest {
        slots: slot_subs,
        accounts: HashMap::new(),
        transactions: HashMap::new(),
        entry: Default::default(),
        blocks: Default::default(),
        blocks_meta: HashMap::new(),
        commitment: Some(yellowstone_grpc_proto::prelude::CommitmentLevel::Processed as i32),
        accounts_data_slice: Default::default(),
        ping: None,
    }
}
