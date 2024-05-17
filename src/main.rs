use std::thread::sleep;
use std::time::Duration;
use serde_json::json;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::response::SlotInfo;
use solana_sdk::commitment_config::CommitmentConfig;
use tokio::select;
use tokio_stream::{Stream, StreamExt};
use tokio_stream::wrappers::{BroadcastStream, ReceiverStream};
use websocket_tungstenite_retry::websocket_stable::{StableWebSocket, WsMessage};
use url::Url;

type Slot = u64;


#[tokio::main(flavor = "multi_thread", worker_threads = 16)]
async fn main() {
    tracing_subscriber::fmt::init();

    let ws_url1 = format!("wss://api.mainnet-beta.solana.com");
    let ws_url2 = format!("wss://mango.rpcpool.com/{MAINNET_API_TOKEN}",
                          MAINNET_API_TOKEN = std::env::var("MAINNET_API_TOKEN").unwrap());
    let rpc_url = format!("https://mango.rpcpool.com/{MAINNET_API_TOKEN}",
                          MAINNET_API_TOKEN = std::env::var("MAINNET_API_TOKEN").unwrap());

    let (mpsc_downstream, mut mpsc_upstream) = tokio::sync::mpsc::channel(100);

    tokio::spawn(websocket_source(Url::parse(ws_url1.as_str()).unwrap(), mpsc_downstream.clone()));
    tokio::spawn(websocket_source(Url::parse(ws_url2.as_str()).unwrap(), mpsc_downstream.clone()));
    tokio::spawn(rpc_getslot_source(Url::parse(rpc_url.as_str()).unwrap(), mpsc_downstream.clone()));

    while let Some(slot) = mpsc_upstream.recv().await {
        println!("Slot: {}", slot);
    }

    sleep(Duration::from_secs(10));
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
        mpsc_downstream.send(slot).await.unwrap();
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
            mpsc_downstream.send(slot_info.slot).await.unwrap();
        }
    }

}