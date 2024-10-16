mod utils;

use crate::utils::configure_panic_hook;
use enum_iterator::Sequence;
use geyser_grpc_connector::grpc_subscription_autoreconnect_streams::create_geyser_reconnecting_stream;
use geyser_grpc_connector::{GrpcConnectionTimeouts, GrpcSourceConfig, Message};
use itertools::Itertools;
use serde_json::json;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::response::SlotInfo;
use solana_sdk::commitment_config::CommitmentConfig;
use std::collections::{HashMap, HashSet};
use std::env;
use std::pin::pin;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::error::Elapsed;
use tokio::time::{sleep, timeout, Instant, Timeout};
use tokio_stream::StreamExt;
use tracing::{error, warn};
use url::Url;
use websocket_tungstenite_retry::websocket_stable::{StableWebSocket, WsMessage};
use yellowstone_grpc_proto::geyser::subscribe_update::UpdateOneof;
use yellowstone_grpc_proto::geyser::{SubscribeRequest, SubscribeRequestFilterSlots};

type Slot = u64;

#[derive(Debug, Clone, Eq, Hash, PartialEq, Sequence)]
enum SlotSource {
    SolanaWebsocket,
    SolanaRpc,
    TritonRpc,
    TritonWebsocket,
    OurGrpcSource,
}

#[allow(dead_code)]
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
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    configure_panic_hook();

    let timeouts = GrpcConnectionTimeouts {
        connect_timeout: Duration::from_secs(10),
        request_timeout: Duration::from_secs(10),
        subscribe_timeout: Duration::from_secs(10),
        receive_timeout: Duration::from_secs(10),
    };

    // the system under test (our system)
    let grpc_addr = std::env::var("GRPC_ADDR").expect("require env variable GRPC_ADDR");
    let grpc_x_token = env::var("GRPC_X_TOKEN").ok();
    let our_grpc_config =
        GrpcSourceConfig::new(grpc_addr.to_string(), grpc_x_token, None, timeouts.clone());

    // the sources to compare against
    let solana_rpc_url = "https://api.mainnet-beta.solana.com".to_string();
    let solana_ws_url = "wss://api.mainnet-beta.solana.com".to_string();
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
    // --

    let (slots_tx, mut slots_rx) = tokio::sync::mpsc::channel::<SlotDatapoint>(100);

    start_our_grpc_geyser_slots_task(
        our_grpc_config.clone(),
        SlotSource::OurGrpcSource,
        slots_tx.clone(),
    );

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
    tokio::spawn(rpc_getslot_source(
        solana_rpc_url,
        SlotSource::SolanaRpc,
        slots_tx.clone(),
    ));
    tokio::spawn(rpc_getslot_source(
        triton_rpc_url,
        SlotSource::TritonRpc,
        slots_tx.clone(),
    ));

    let mut latest_slot_per_source: HashMap<SlotSource, Slot> = HashMap::new();
    let mut update_timestamp_per_source: HashMap<SlotSource, Instant> = HashMap::new();

    while let Some(SlotDatapoint {
        slot,
        source,
        timestamp: update_timestamp,
    }) = slots_rx.recv().await
    {
        // println!("Slot from {:?}: {}", source, slot);
        latest_slot_per_source.insert(source.clone(), slot);
        update_timestamp_per_source.insert(source.clone(), update_timestamp);

        visualize_slots(&latest_slot_per_source, &update_timestamp_per_source).await;

        // if Instant::now().duration_since(started_at) > Duration::from_secs(10) {
        //     break;
        // }
    }
}

async fn rpc_getslot_source(
    rpc_url: Url,
    slot_source: SlotSource,
    mpsc_downstream: tokio::sync::mpsc::Sender<SlotDatapoint>,
) {
    let rpc = RpcClient::new_with_timeout(rpc_url.to_string(), Duration::from_secs(5));
    loop {
        tokio::time::sleep(Duration::from_millis(800)).await;
        let res = rpc
            .get_slot_with_commitment(CommitmentConfig::processed())
            .await;

        match res {
            Ok(slot) => {
                match mpsc_downstream
                    .send(SlotDatapoint::new(slot_source.clone(), slot))
                    .await
                {
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
            Err(err) => {
                println!("Error getting slot: {:?} - retry", err);
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

async fn websocket_source(
    rpc_url: Url,
    slot_source: SlotSource,
    mpsc_downstream: tokio::sync::mpsc::Sender<SlotDatapoint>,
) {
    let processed_slot_subscribe = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "slotSubscribe",
    });

    let mut ws1 = StableWebSocket::new_with_timeout(
        rpc_url,
        processed_slot_subscribe.clone(),
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    let mut channel = ws1.subscribe_message_channel();

    // the timeout is a workaround as we see the websocket source starving with no data
    loop {
        match timeout(Duration::from_millis(5000), channel.recv()).await {
            Ok(Ok(WsMessage::Text(payload))) => {
                let ws_result: jsonrpsee_types::SubscriptionResponse<SlotInfo> =
                    serde_json::from_str(&payload).unwrap();
                let slot_info = ws_result.params.result;
                match mpsc_downstream
                    .send(SlotDatapoint::new(slot_source.clone(), slot_info.slot))
                    .await
                {
                    Ok(_) => {}
                    Err(_) => panic!("downstream error"),
                }
            }
            Ok(Ok(WsMessage::Binary(_))) => {
                panic!("Unexpected binary message from websocket source");
            }
            Err(_elapsed) => {
                warn!("Websocket source timeout unexpectedly; continue waiting but there's little hope");
                // throttle a bit
                sleep(Duration::from_millis(500)).await;
            }
            Ok(Err(RecvError::Lagged(_))) => {
                warn!("Websocket broadcast channel lagged - continue");
            }
            Ok(Err(RecvError::Closed)) => {
                panic!("Websocket broadcast channel closed - should never happen");
            }
        }
    }

    // unreachable code
}

// note: this might fail if the yellowstone plugin does not allow "any broadcast filter"
fn start_our_grpc_geyser_slots_task(
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
                    match mpsc_downstream
                        .send(SlotDatapoint::new(slot_source.clone(), slot_info.slot))
                        .await
                    {
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

const STALE_SOURCE_TIMEOUT: Duration = Duration::from_millis(3000);

async fn visualize_slots(
    latest_slot_per_source: &HashMap<SlotSource, Slot>,
    update_timestamp_per_source: &HashMap<SlotSource, Instant>,
) {
    // println!("Slots: {:?}", latest_slot_per_source);

    let threshold = Instant::now() - STALE_SOURCE_TIMEOUT;

    let stale_sources: HashSet<SlotSource> = update_timestamp_per_source
        .iter()
        .filter(|(source, &updated_timestamp)| updated_timestamp < threshold)
        .map(|(source, _)| source)
        .cloned()
        .collect();

    let map_source_by_name: HashMap<String, SlotSource> = enum_iterator::all::<SlotSource>()
        .map(|check| (format!("{:?}", check), check))
        .collect();

    let sorted_by_time: Vec<(&SlotSource, &Slot)> = latest_slot_per_source
        .iter()
        .sorted_by_key(|(_, slot)| *slot)
        .collect_vec();
    let deltas = sorted_by_time
        .windows(2)
        .map(|window| {
            let (_source1, slot1) = window[0];
            let (_source2, slot2) = window[1];
            slot2 - slot1
        })
        .collect_vec();

    for i in 0..(sorted_by_time.len() + deltas.len()) {
        if i % 2 == 0 {
            let (source, slot) = sorted_by_time.get(i / 2).unwrap();
            let staleness_marker = if stale_sources.contains(source) {
                "!!"
            } else {
                ""
            };
            print!("{staleness_marker}{slot}({source:?}){staleness_marker}");
        } else {
            let edge = *deltas.get(i / 2).unwrap();
            if edge == 0 {
                print!(" = ");
            } else if edge < 20 {
                print!(" {} ", ".".repeat(edge as usize));
            } else {
                print!(" ...>>{}>>... ", edge);
            }
        }
    }

    let all_sources: HashSet<SlotSource> = map_source_by_name.values().cloned().collect();
    let sources_with_data: HashSet<SlotSource> = latest_slot_per_source.keys().cloned().collect();

    let no_data_sources = all_sources.difference(&sources_with_data).collect_vec();
    if no_data_sources.is_empty() {
        print!(" // all sources have data");
    } else {
        print!(" // no data from {:?}", no_data_sources);
    }

    if stale_sources.is_empty() {
        print!(", no stale sources");
    } else {
        print!(
            ", {} stale sources (threshold={:?})",
            stale_sources.len(),
            STALE_SOURCE_TIMEOUT
        );
    }

    println!();
    // print!("{}[2K\r", 27 as char);
}
