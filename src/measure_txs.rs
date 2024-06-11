use anyhow::{bail, Result};
use futures_util::future::join_all;
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
    lamports: u64,
) -> Result<TxSendResult> {
    info!("sending tx to {}", label);
    let user_pub = user.pubkey();
    let transfer_ix: Instruction = transfer(&user_pub, &user_pub, lamports);
    let compute_budget_ix: Instruction = ComputeBudgetInstruction::set_compute_unit_limit(50_000);
    let compute_price_ix: Instruction = ComputeBudgetInstruction::set_compute_unit_price(100_000);
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
