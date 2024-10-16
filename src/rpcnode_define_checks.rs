use crate::TASK_TIMEOUT;
use anyhow::Context;
use enum_iterator::Sequence;
use futures_util::{FutureExt, StreamExt};
use geyser_grpc_connector::grpc_subscription_autoreconnect_streams::create_geyser_reconnecting_stream;
use geyser_grpc_connector::{GrpcConnectionTimeouts, GrpcSourceConfig, Message};
use serde_json::json;
use solana_account_decoder::parse_token::spl_token_ids;
use solana_account_decoder::{UiAccountEncoding, UiDataSliceConfig};
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client::rpc_client::GetConfirmedSignaturesForAddress2Config;
use solana_rpc_client_api::config::{RpcAccountInfoConfig, RpcProgramAccountsConfig};
use solana_rpc_client_api::request::TokenAccountsFilter;
use solana_sdk::clock::Slot;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::future::Future;
use std::pin::pin;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::sync::oneshot::Sender;
use tokio::task::JoinSet;
use tokio::time::{timeout, Instant};
use tracing::{debug, error, info};
use url::Url;
use websocket_tungstenite_retry::websocket_stable::{StableWebSocket, WsMessage};
use yellowstone_grpc_proto::geyser::subscribe_update::UpdateOneof;
use yellowstone_grpc_proto::geyser::{
    SubscribeRequest, SubscribeRequestFilterAccounts, SubscribeRequestFilterSlots,
};

#[derive(Clone, Debug, PartialEq, Sequence)]
pub enum Check {
    RpcGpa,
    RpcTokenAccounts,
    RpcGsfa,
    RpcGetAccountInfo,
    GeyserAllAccounts,
    GeyserTokenAccount,
    WebsocketAccount,
    SlotLagging,
}

pub enum CheckResult {
    Success(Check),
    Timeout(Check),
}

pub fn define_checks(checks_enabled: &[Check], all_check_tasks: &mut JoinSet<CheckResult>) {
    if checks_enabled.contains(&Check::RpcGpa) {
        let rpc_client = read_rpc_config();
        add_task(Check::RpcGpa, rpc_gpa(rpc_client.clone()), all_check_tasks);
    }
    if checks_enabled.contains(&Check::RpcTokenAccounts) {
        let rpc_client = read_rpc_config();
        add_task(
            Check::RpcTokenAccounts,
            rpc_get_token_accounts_by_owner(rpc_client.clone()),
            all_check_tasks,
        );
    }
    if checks_enabled.contains(&Check::RpcGsfa) {
        let rpc_client = read_rpc_config();
        add_task(
            Check::RpcGsfa,
            rpc_get_signatures_for_address(rpc_client.clone()),
            all_check_tasks,
        );
    }
    if checks_enabled.contains(&Check::RpcGetAccountInfo) {
        let rpc_client = read_rpc_config();
        add_task(
            Check::RpcGetAccountInfo,
            rpc_get_account_info(rpc_client.clone()),
            all_check_tasks,
        );
    }

    if checks_enabled.contains(&Check::GeyserAllAccounts) {
        let geyser_grpc_config = read_geyser_config();
        add_task(
            Check::GeyserAllAccounts,
            create_geyser_all_accounts_task(geyser_grpc_config),
            all_check_tasks,
        );
    }
    if checks_enabled.contains(&Check::GeyserTokenAccount) {
        let geyser_grpc_config = read_geyser_config();
        add_task(
            Check::GeyserTokenAccount,
            create_geyser_token_account_task(geyser_grpc_config),
            all_check_tasks,
        );
    }
    if checks_enabled.contains(&Check::WebsocketAccount) {
        let ws_addr = read_ws_config();
        add_task(
            Check::WebsocketAccount,
            websocket_account_subscribe(Url::parse(ws_addr.as_str()).unwrap()),
            all_check_tasks,
        );
    }
    if checks_enabled.contains(&Check::SlotLagging) {
        let geyser_grpc_config = read_geyser_config();
        let reference_rpc_url = reference_rpc_url();
        add_task(
            Check::SlotLagging,
            slot_latency_check(geyser_grpc_config, reference_rpc_url),
            all_check_tasks,
        );
    }
}

fn read_rpc_config() -> Arc<RpcClient> {
    // http://...
    let rpc_addr = std::env::var("RPC_HTTP_ADDR").unwrap();

    let rpc_url = Url::parse(rpc_addr.as_str()).unwrap();

    Arc::new(RpcClient::new(rpc_url.to_string()))
}

fn reference_rpc_url() -> Arc<RpcClient> {
    const FALLBACK_RPC: &str = "https://api.mainnet-beta.solana.com";
    // http://...
    let rpc_addr = std::env::var("REFERENCE_RPC_HTTP_ADDR").unwrap_or(FALLBACK_RPC.to_string());

    let rpc_url = Url::parse(rpc_addr.as_str()).unwrap();

    Arc::new(RpcClient::new(rpc_url.to_string()))
}

fn read_ws_config() -> String {
    // wss://...

    std::env::var("RPC_WS_ADDR").unwrap()
}

fn read_geyser_config() -> GrpcSourceConfig {
    let grpc_addr = std::env::var("GRPC_ADDR").unwrap();
    let grpc_x_token = std::env::var("GRPC_X_TOKEN").ok();

    let geyser_grpc_timeouts = GrpcConnectionTimeouts {
        connect_timeout: Duration::from_secs(10),
        request_timeout: Duration::from_secs(10),
        subscribe_timeout: Duration::from_secs(10),
        receive_timeout: Duration::from_secs(10),
    };

    GrpcSourceConfig::new(
        grpc_addr.to_string(),
        grpc_x_token,
        None,
        geyser_grpc_timeouts.clone(),
    )
}

fn add_task(
    check: Check,
    task: impl Future<Output = ()> + Send + 'static,
    all_check_tasks: &mut JoinSet<CheckResult>,
) {
    let timeout = timeout(TASK_TIMEOUT, task).then(|res| async move {
        match res {
            Ok(()) => CheckResult::Success(check),
            Err(_) => CheckResult::Timeout(check),
        }
    });
    all_check_tasks.spawn(timeout);
}

// note: this might fail if the yellowstone plugin does not allow "any broadcast filter"
async fn create_geyser_all_accounts_task(config: GrpcSourceConfig) {
    let green_stream = create_geyser_reconnecting_stream(config.clone(), all_accounts());

    let mut count = 0;
    let mut green_stream = pin!(green_stream);
    while let Some(message) = green_stream.next().await {
        if let Message::GeyserSubscribeUpdate(subscriber_update) = message {
            if let Some(UpdateOneof::Account(update)) = subscriber_update.update_oneof {
                debug!("Account from geyser: {:?}", update.account.unwrap().pubkey);
                count += 1;
                if count > 3 {
                    return;
                }
            }
        }
    }

    panic!("failed to receive the requested accounts");
}

async fn create_geyser_token_account_task(config: GrpcSourceConfig) {
    let green_stream = create_geyser_reconnecting_stream(config.clone(), token_accounts());

    let mut count = 0;
    let mut green_stream = pin!(green_stream);
    while let Some(message) = green_stream.next().await {
        if let Message::GeyserSubscribeUpdate(subscriber_update) = message {
            if let Some(UpdateOneof::Account(update)) = subscriber_update.update_oneof {
                debug!("Token Account: {:?}", update.account.unwrap().pubkey);
                count += 1;
                if count > 3 {
                    return;
                }
            }
        }
    }

    panic!("failed to receive the requested token accounts");
}

async fn rpc_gpa(rpc_client: Arc<RpcClient>) {
    let program_pubkey = Pubkey::from_str("4MangoMjqJ2firMokCjjGgoK8d4MXcrgL7XJaL3w6fVg").unwrap();

    let _config = RpcProgramAccountsConfig {
        filters: None,
        account_config: RpcAccountInfoConfig {
            encoding: Some(UiAccountEncoding::Base64),
            data_slice: Some(UiDataSliceConfig {
                offset: 0,
                length: 4,
            }),
            commitment: None,
            min_context_slot: None,
        },
        with_context: None,
    };

    let program_accounts = rpc_client
        .get_program_accounts(&program_pubkey)
        .await
        .context("rpc_gpa")
        .unwrap();

    // info!("hex {:02X?}", program_accounts[0].1.data);

    debug!("Program accounts: {:?}", program_accounts.len());
    // mango 12400 on mainnet

    assert!(
        program_accounts.len() > 1000,
        "program accounts count is too low"
    );
}

async fn rpc_get_account_info(rpc_client: Arc<RpcClient>) {
    let program_pubkey = Pubkey::from_str("4MangoMjqJ2firMokCjjGgoK8d4MXcrgL7XJaL3w6fVg").unwrap();

    let account_info = rpc_client.get_account(&program_pubkey).await.unwrap();

    debug!("Account info: {:?}", account_info);

    assert!(account_info.lamports > 0, "account lamports is zero");
}

async fn rpc_get_token_accounts_by_owner(rpc_client: Arc<RpcClient>) {
    let owner_pubkey = Pubkey::from_str("gmgLgwHZbRxbPHuGtE2cVVAXL6yrS8SvvFkDNjmWfkj").unwrap();
    let mint_usdc: Pubkey =
        Pubkey::from_str("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").unwrap();

    let token_accounts = rpc_client
        .get_token_accounts_by_owner(&owner_pubkey, TokenAccountsFilter::Mint(mint_usdc))
        .await
        .context("rpc_get_token_accounts_by_owner")
        .unwrap();

    // 1 account
    debug!("Token accounts: {:?}", token_accounts.len());

    assert!(!token_accounts.is_empty(), "token accounts count is zero");
}

async fn rpc_get_signatures_for_address(rpc_client: Arc<RpcClient>) {
    let address = Pubkey::from_str("Vote111111111111111111111111111111111111111").unwrap();

    let config = GetConfirmedSignaturesForAddress2Config {
        before: None,
        until: None,
        limit: Some(10),
        commitment: Some(CommitmentConfig::confirmed()),
    };

    let signatures = rpc_client
        .get_signatures_for_address_with_config(&address, config)
        .await
        .context("get_signatures_for_address_with_config")
        .unwrap();

    debug!("Signatures for Address {}: {:?}", address, signatures.len());

    assert!(signatures.len() >= 10, "signatures count is too low");
}

async fn websocket_account_subscribe(rpc_url: Url) {
    let sysvar_subscribe = json!({
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
    .context("new websocket")
    .unwrap();

    let mut channel = ws1.subscribe_message_channel();

    let mut count = 0;
    while let Ok(msg) = channel.recv().await {
        if let WsMessage::Text(payload) = msg {
            debug!("SysvarC1ock: {:?}", payload);
            count += 1;
        }
        if count > 3 {
            return;
        }
    }

    panic!("failed to receive the requested sysvar clock accounts");
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
            owner: spl_token_ids()
                .iter()
                .map(|pubkey| pubkey.to_string())
                .collect(),
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

fn slots() -> SubscribeRequest {
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

async fn rpc_getslot_from_rpc(rpc: Arc<RpcClient>) -> Slot {
    loop {
        let res = rpc
            .get_slot_with_commitment(CommitmentConfig::processed())
            .await;

        match res {
            Ok(slot) => {
                debug!("Reference slot: {:?}", slot);
                return slot as Slot;
            }
            Err(err) => {
                error!("Error getting slot: {:?} - retry", err);
            }
        }
        tokio::time::sleep(Duration::from_millis(800)).await;
    }
}

async fn spawn_get_our_grpc_geyser_slot(config: GrpcSourceConfig, oneshot: Sender<u64>) {
    let started_at = Instant::now();
    let geyser_stream = create_geyser_reconnecting_stream(config.clone(), slots());

    tokio::spawn(async move {
        let mut geyser_stream = pin!(geyser_stream);
        while let Some(message) = geyser_stream.next().await {
            if let Message::GeyserSubscribeUpdate(subscriber_update) = message {
                if let Some(UpdateOneof::Slot(slot_info)) = subscriber_update.update_oneof {
                    debug!(
                        "Geyser slot read in {:.2}ms",
                        started_at.elapsed().as_secs_f64() * 1000.0
                    );
                    oneshot.send(slot_info.slot as Slot).unwrap();
                    return;
                }
            }
        }
    });
}

async fn slot_latency_check(geyser_grpc_config: GrpcSourceConfig, ref_rpc_client: Arc<RpcClient>) {
    let (tx, rx) = oneshot::channel();
    spawn_get_our_grpc_geyser_slot(geyser_grpc_config, tx).await;

    let our_slot = rx.await.unwrap();
    let upper_slot = rpc_getslot_from_rpc(ref_rpc_client.clone()).await;

    debug!("Our slot: {}, Upper slot: {}", our_slot, upper_slot);

    // Our slot: 294879057, Upper slot: 294879058

    let lag = our_slot.saturating_sub(upper_slot).max(0);

    info!("Slot lag: {} (us:{} them:{})", lag, our_slot, upper_slot);

    const SLOT_LAG_ERROR_THRESHOLD: u64 = 10;

    assert!(
        lag < SLOT_LAG_ERROR_THRESHOLD,
        "Slot lag was too high (us:{} them:{})",
        our_slot,
        upper_slot
    );
}
