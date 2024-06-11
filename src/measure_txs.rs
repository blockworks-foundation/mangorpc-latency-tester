use anyhow::{bail, Result};
use futures_util::future::join_all;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use solana_client::{
    nonblocking::{pubsub_client::PubsubClient, rpc_client::RpcClient},
    rpc_response::SlotUpdate,
};
use solana_program::system_instruction::transfer;
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    hash::Hash,
    instruction::Instruction,
    message::Message,
    signature::{Keypair, Signature},
    signer::Signer,
    transaction::Transaction,
};
use solana_transaction_status::UiTransactionEncoding;
use std::{
    collections::HashMap,
    iter::zip,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    sync::Notify,
    task::JoinHandle,
    time::{sleep, timeout},
};
use tokio_stream::StreamExt;
use tracing::{debug, error, info, warn};

#[derive(Debug)]
pub struct TxSendResult {
    pub label: String,
    pub signature: Signature,
    pub slot_sent: u64,
    pub slot_confirmed: u64,
}

#[derive(Serialize)]
struct RequestParams {
    jsonrpc: String,
    id: String,
    method: String,
    params: Vec<Params>,
}

#[derive(Serialize)]
struct Params {
    options: Options,
}

#[derive(Serialize)]
struct Options {
    priority_level: String,
}

#[derive(Deserialize)]
struct ResponseData {
    result: ResultData,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResultData {
    priority_fee_estimate: f64,
}

async fn get_priority_fee_estimate(helius_url: &str) -> Result<u64> {
    let client = Client::new();

    let request_body = RequestParams {
        jsonrpc: "2.0".to_string(),
        id: "1".to_string(),
        method: "getPriorityFeeEstimate".to_string(),
        params: vec![Params {
            options: Options {
                priority_level: "High".to_string(),
            },
        }],
    };

    let response = client.post(helius_url).json(&request_body).send().await?;

    let response_text = response.text().await?;

    debug!("prio fee res: {}", response_text);

    let response_data: ResponseData = serde_json::from_str(&response_text)?;

    Ok(response_data.result.priority_fee_estimate.floor() as u64)
}

async fn watch_slots(
    ps_url: String,
    atomic_slot: Arc<AtomicU64>,
    slot_notifier: Arc<Notify>,
) -> Result<()> {
    let ps_client: PubsubClient = PubsubClient::new(&ps_url).await?;
    let (mut sub, _unsub) = ps_client.slot_updates_subscribe().await?;
    let slot_timeout_threshold = Duration::from_millis(800);

    loop {
        match timeout(slot_timeout_threshold, sub.next()).await {
            Ok(Some(SlotUpdate::FirstShredReceived { slot, .. })) => {
                atomic_slot.store(slot, Ordering::Relaxed);
                slot_notifier.notify_one();
                debug!("slot_subscribe response: {:?}", slot);
            }
            Ok(Some(_)) => (), // we don't care about non-first-shred-received updates
            Ok(None) => bail!("slot_subscribe stream ended"),
            Err(_) => bail!(
                "watch_slots: no update in {}ms",
                slot_timeout_threshold.as_millis()
            ),
        }
    }
}

pub fn watch_slots_retry(
    ps_url: String,
    atomic_slot: Arc<AtomicU64>,
    slot_notifier: Arc<Notify>,
) -> JoinHandle<()> {
    let handle = tokio::spawn(async move {
        loop {
            let ps_url = ps_url.clone();
            let a_slot = Arc::clone(&atomic_slot);
            let s_notifier = Arc::clone(&slot_notifier);
            match watch_slots(ps_url, a_slot, s_notifier).await {
                Ok(()) => warn!("watch_slots ended without an error. restarting..."),
                Err(e) => {
                    error!("watch_slots error: {:?}", e);
                    sleep(Duration::from_secs(5)).await;
                }
            }
        }
    });

    handle
}

async fn send_and_confirm_self_transfer_tx(
    user: Arc<Keypair>,
    atomic_slot: Arc<AtomicU64>,
    label: String,
    rpc_client: Arc<RpcClient>,
    recent_blockhash: Hash,
    priority_fee: u64,
    lamports: u64,
) -> Result<TxSendResult> {
    info!("sending tx to {}", label);
    let user_pub = user.pubkey();
    let transfer_ix: Instruction = transfer(&user_pub, &user_pub, lamports);
    let compute_budget_ix: Instruction = ComputeBudgetInstruction::set_compute_unit_limit(50_000);
    let compute_price_ix: Instruction =
        ComputeBudgetInstruction::set_compute_unit_price(priority_fee);
    let message = Message::new(
        &[transfer_ix, compute_budget_ix, compute_price_ix],
        Some(&user_pub),
    );

    let tx = Transaction::new(&[&user], message, recent_blockhash);

    let slot_sent = atomic_slot.load(Ordering::Relaxed);
    let signature = rpc_client.send_and_confirm_transaction(&tx).await?;
    sleep(Duration::from_secs(5)).await;
    let slot_confirmed = rpc_client
        .get_transaction(&signature, UiTransactionEncoding::Json)
        .await?
        .slot;

    info!("tx confirmed for {}", label);

    Ok(TxSendResult {
        label,
        signature,
        slot_sent,
        slot_confirmed,
    })
}

pub async fn measure_txs(
    user: Keypair,
    pubsub_url: String,
    rpc_url: String,
    helius_url: String,
    urls_by_label: HashMap<String, String>,
) -> Result<()> {
    info!("measuring txs...");
    let user = Arc::new(user);
    let atomic_slot = AtomicU64::new(0);
    let atomic_slot = Arc::new(atomic_slot);
    let slot_notifier = Arc::new(Notify::new());

    let a_slot = Arc::clone(&atomic_slot);
    let s_notifier = Arc::clone(&slot_notifier);
    let _handle = watch_slots_retry(pubsub_url, a_slot, s_notifier);

    info!("waiting for first slot...");
    slot_notifier.notified().await;

    let mut clients_by_label = HashMap::<String, Arc<RpcClient>>::new();
    for (label, url) in urls_by_label.into_iter() {
        let rpc_client = RpcClient::new(url);
        clients_by_label.insert(label, Arc::new(rpc_client));
    }

    let rpc_client = RpcClient::new(rpc_url);
    let recent_blockhash = rpc_client.get_latest_blockhash().await?;
    info!("got recent_blockhash: {}", recent_blockhash);

    let priority_fee = get_priority_fee_estimate(&helius_url).await?;
    info!("using priority fee: {}", priority_fee);

    let mut sig_futs = Vec::new();
    for (i, (label, client)) in clients_by_label.iter().enumerate() {
        let label = label.clone();
        let rpc_client = Arc::clone(client);
        let user = Arc::clone(&user);
        let a_slot = Arc::clone(&atomic_slot);
        let fut = send_and_confirm_self_transfer_tx(
            user,
            a_slot,
            label,
            rpc_client,
            recent_blockhash,
            priority_fee,
            i as u64,
        );
        sig_futs.push(fut);
    }

    let results = join_all(sig_futs).await;
    for ((label, _), r) in zip(clients_by_label, results) {
        if let Ok(TxSendResult {
            label,
            signature,
            slot_sent,
            slot_confirmed,
        }) = r
        {
            info!("label: {}", label);
            info!("txSig: https://solscan.io/tx/{}", signature);
            info!("slot_sent: {}", slot_sent);
            info!("slot_confirmed: {}", slot_confirmed);
            info!("slot_diff: {}", slot_confirmed - slot_sent);
        } else {
            error!("label: {}", label);
            error!("error: {:?}", r);
        }
    }

    Ok(())
}

pub async fn watch_measure_txs(
    user: Keypair,
    pubsub_url: String,
    rpc_url: String,
    helius_url: String,
    urls_by_label: HashMap<String, String>,
    watch_interval_seconds: u64,
) -> Result<()> {
    info!("measuring txs...");
    let user = Arc::new(user);
    let atomic_slot = AtomicU64::new(0);
    let atomic_slot = Arc::new(atomic_slot);
    let slot_notifier = Arc::new(Notify::new());

    let a_slot = Arc::clone(&atomic_slot);
    let s_notifier = Arc::clone(&slot_notifier);
    let _handle = watch_slots_retry(pubsub_url, a_slot, s_notifier);

    info!("waiting for first slot...");
    slot_notifier.notified().await;

    let mut clients_by_label = HashMap::<String, Arc<RpcClient>>::new();
    for (label, url) in urls_by_label.into_iter() {
        let rpc_client = RpcClient::new(url);
        clients_by_label.insert(label, Arc::new(rpc_client));
    }

    let rpc_client = RpcClient::new(rpc_url);

    loop {
        let recent_blockhash = rpc_client.get_latest_blockhash().await?;
        info!("got recent_blockhash: {}", recent_blockhash);

        let priority_fee = get_priority_fee_estimate(&helius_url).await?;
        info!("using priority fee: {}", priority_fee);

        let mut sig_futs = Vec::new();
        let c_by_l = clients_by_label.clone();
        for (i, (label, client)) in c_by_l.iter().enumerate() {
            let label = label.clone();
            let rpc_client = Arc::clone(client);
            let user = Arc::clone(&user);
            let a_slot = Arc::clone(&atomic_slot);
            let fut = send_and_confirm_self_transfer_tx(
                user,
                a_slot,
                label,
                rpc_client,
                recent_blockhash,
                priority_fee,
                i as u64,
            );
            sig_futs.push(fut);
        }

        let results = join_all(sig_futs).await;
        for ((label, _), r) in zip(c_by_l, results) {
            if let Ok(TxSendResult {
                label,
                signature,
                slot_sent,
                slot_confirmed,
            }) = r
            {
                info!("label: {}", label);
                info!("txSig: https://solscan.io/tx/{}", signature);
                info!("slot_sent: {}", slot_sent);
                info!("slot_confirmed: {}", slot_confirmed);
                info!("slot_diff: {}", slot_confirmed - slot_sent);
            } else {
                error!("label: {}", label);
                error!("error: {:?}", r);
            }
        }

        sleep(Duration::from_secs(watch_interval_seconds)).await;
    }
}
