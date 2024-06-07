use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::pin;
use std::process::{exit, ExitCode};
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
use tokio::task::{JoinError, JoinHandle, JoinSet};
use tokio::time::{Instant, timeout};
use tokio_stream::{Stream, StreamExt};
use tokio_stream::wrappers::{BroadcastStream, ReceiverStream};
use tracing::{debug, error, info, trace, warn};
use websocket_tungstenite_retry::websocket_stable::{StableWebSocket, WsMessage};
use url::Url;
use yellowstone_grpc_proto::geyser::{SubscribeRequest, SubscribeRequestFilterAccounts, SubscribeRequestFilterSlots, SubscribeUpdate};
use yellowstone_grpc_proto::geyser::subscribe_update::UpdateOneof;
use anyhow::Context;
use enum_iterator::Sequence;
use solana_account_decoder::UiAccountEncoding;
use solana_rpc_client_api::config::{RpcAccountInfoConfig, RpcProgramAccountsConfig};
use solana_rpc_client_api::filter::{Memcmp, RpcFilterType};
use itertools::Itertools;

type Slot = u64;

const TASK_TIMEOUT: Duration = Duration::from_millis(15000);

enum CheckResult {
    Success(Check),
    Timeout(Check),
}

#[derive(Clone, Debug, PartialEq, Sequence)]
enum Check {
    Gpa,
    TokenAccouns,
    Gsfa,
    GetAccountInfo,
    GeyserAllAccounts,
    GeyserTokenAccount,
    WebsocketAccount,
}


async fn send_webook_discord() {
    let url = std::env::var("DISCORD_WEBHOOK").unwrap();
    let client = reqwest::Client::new();
    let res = client.post(url)
        .json(&json!({
            "content": "Hello, World!"
        }))
        .send().await;
    match res {
        Ok(_) => {
            info!("webhook sent");
        }
        Err(e) => {
            error!("webhook failed: {:?}", e);
        }
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 16)]
async fn main() -> ExitCode {
    tracing_subscriber::fmt::init();


    // send_webook_discord().await;

    // name of rpc node for logging/discord (e.g. hostname)
    let rpcnode_label = std::env::var("RPCNODE_LABEL").unwrap();

    let map_checks_by_name: HashMap<String, Check> =
        enum_iterator::all::<Check>().map(|check| {
            (format!("{:?}", check), check)
        }).collect();

    // comma separated
    let checks_enabled = std::env::var("CHECKS_ENABLED").unwrap();
    debug!("checks_enabled unparsed: {}", checks_enabled);

    let checks_enabled: Vec<Check> = checks_enabled.split(",").map(|s| {
        let s = s.trim();

        match map_checks_by_name.get(s) {
            Some(check) => check,
            None => {
                error!("unknown check: {}", s);
                exit(1);
            }
        }
    }).cloned().collect_vec();

    info!("checks enabled for rpcnode <{}>: {:?}", rpcnode_label, checks_enabled);





    let mut all_check_tasks: JoinSet<CheckResult> = JoinSet::new();

    if checks_enabled.contains(&Check::Gpa) {
        let rpc_client = read_rpc_config();
        add_task(Check::Gpa, rpc_gpa(rpc_client.clone()), &mut all_check_tasks);
    }
    if checks_enabled.contains(&Check::TokenAccouns) {
        let rpc_client = read_rpc_config();
        add_task(Check::TokenAccouns, rpc_get_token_accounts_by_owner(rpc_client.clone()), &mut all_check_tasks);
    }
    if checks_enabled.contains(&Check::Gsfa) {
        let rpc_client = read_rpc_config();
        add_task(Check::Gsfa, rpc_get_signatures_for_address(rpc_client.clone()), &mut all_check_tasks);
    }
    if checks_enabled.contains(&Check::GetAccountInfo) {
        let rpc_client = read_rpc_config();
        add_task(Check::GetAccountInfo, rpc_get_account_info(rpc_client.clone()), &mut all_check_tasks);
    }

    if checks_enabled.contains(&Check::GeyserAllAccounts) {
        let geyser_grpc_config = read_geyser_config();
        add_task(Check::GeyserAllAccounts, create_geyser_all_accounts_task(geyser_grpc_config), &mut all_check_tasks);
    }
    if checks_enabled.contains(&Check::GeyserTokenAccount) {
        let geyser_grpc_config = read_geyser_config();
        add_task(Check::GeyserTokenAccount, create_geyser_token_account_task(geyser_grpc_config), &mut all_check_tasks);
    }
    if checks_enabled.contains(&Check::WebsocketAccount) {
        let ws_addr = read_ws_config();
        add_task(Check::WebsocketAccount, websocket_account_subscribe(Url::parse(ws_addr.as_str()).unwrap()), &mut all_check_tasks);
    }

    let tasks_total = all_check_tasks.len();
    info!("all {} tasks started...", tasks_total);

    let mut tasks_success = Vec::new();
    let mut tasks_successful = 0;
    let mut tasks_timeout = 0;
    let mut tasks_failed = 0;
    while let Some(res) = all_check_tasks.join_next().await {
        match res {
            Ok(CheckResult::Success(check)) => {
                tasks_successful += 1;
                info!("one more task completed <{:?}>, {}/{} left", check, all_check_tasks.len(), tasks_total);
                tasks_success.push(check);
            }
            Ok(CheckResult::Timeout(check)) => {
                tasks_timeout += 1;
                warn!("timeout running task <{:?}>", check);
            }
            Err(_) => {
                tasks_failed += 1;
                warn!("Task execution failed");
            }
        }
    }
    let tasks_total = tasks_successful + tasks_failed + tasks_timeout;

    assert!(tasks_total > 0, "no results");


    if tasks_failed + tasks_timeout > 0 {
        warn!("rpcnode <{}> - tasks failed ({}) or timed out ({}) of {} total",
            rpcnode_label, tasks_failed, tasks_timeout, tasks_total);
        for check in enum_iterator::all::<Check>() {
            if !tasks_success.contains(&check) {
                warn!("!! did not complet task <{:?}>", check);
            }
        }
        return ExitCode::SUCCESS;
    } else {
        info!("rpcnode <{}> - all {} tasks completed: {:?}", rpcnode_label, tasks_total, tasks_success);
        return ExitCode::FAILURE;
    }
}


fn read_rpc_config() -> Arc<RpcClient> {
    // http://...
    let rpc_addr = std::env::var("RPC_HTTP_ADDR").unwrap();

    let rpc_url = Url::parse(rpc_addr.as_str()).unwrap();
    let rpc_client = Arc::new(RpcClient::new(rpc_url.to_string()));

    rpc_client
}

fn read_ws_config() -> String {
    // wss://...
    let ws_addr = std::env::var("RPC_WS_ADDR").unwrap();

    ws_addr
}

fn read_geyser_config() -> GrpcSourceConfig {
    let grpc_addr = std::env::var("GRPC_ADDR").unwrap();

    let geyser_grpc_timeouts = GrpcConnectionTimeouts {
        connect_timeout: Duration::from_secs(10),
        request_timeout: Duration::from_secs(10),
        subscribe_timeout: Duration::from_secs(10),
        receive_timeout: Duration::from_secs(10),
    };
    let geyser_grpc_config = GrpcSourceConfig::new(grpc_addr.to_string(), None, None, geyser_grpc_timeouts.clone());

    geyser_grpc_config
}

fn add_task(check: Check, task: impl Future<Output=()> + Send + 'static, all_check_tasks: &mut JoinSet<CheckResult>) {
    let timeout =
        timeout(TASK_TIMEOUT, task).then(|res| async move {
            match res {
                Ok(()) => {
                    CheckResult::Success(check)
                }
                Err(_) => {
                    CheckResult::Timeout(check)
                }
            }
        });
    all_check_tasks.spawn(timeout);
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
                        debug!("Account from geyser: {:?}", update.account.unwrap().pubkey);
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

    panic!("failed to receive the requested accounts");
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
                        debug!("Token Account: {:?}", update.account.unwrap().pubkey);
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

    panic!("failed to receive the requested token accounts");
}

async fn rpc_gpa(rpc_client: Arc<RpcClient>) {
    let program_pubkey = Pubkey::from_str("4MangoMjqJ2firMokCjjGgoK8d4MXcrgL7XJaL3w6fVg").unwrap();

    let filter = RpcFilterType::Memcmp(Memcmp::new_raw_bytes(30, vec![42]));

    let _config = RpcProgramAccountsConfig {
        // filters: Some(vec![filter]),
        filters: None,
        account_config: RpcAccountInfoConfig {
            encoding: None,
            data_slice: None,
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

    assert!(program_accounts.len() > 1000, "program accounts count is too low");
}

async fn rpc_get_account_info(rpc_client: Arc<RpcClient>) {
    let program_pubkey = Pubkey::from_str("4MangoMjqJ2firMokCjjGgoK8d4MXcrgL7XJaL3w6fVg").unwrap();

    let account_info = rpc_client
        .get_account(&program_pubkey)
        .await
        .unwrap();

    debug!("Account info: {:?}", account_info);

    assert!(account_info.lamports > 0, "account lamports is zero");
}

async fn rpc_get_token_accounts_by_owner(rpc_client: Arc<RpcClient>) {
    let owner_pubkey = Pubkey::from_str("whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc").unwrap();
    let mint = Pubkey::from_str("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").unwrap();

    let token_accounts = rpc_client
        .get_token_accounts_by_owner(
            &owner_pubkey,
            TokenAccountsFilter::Mint(mint),
        )
        .await
        .context("rpc_get_token_accounts_by_owner")
        .unwrap();

    // 1 account
    debug!("Token accounts: {:?}", token_accounts.len());

    assert!(token_accounts.len() > 0, "token accounts count is zero");
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
        .context("get_signatures_for_address_with_config")
        .unwrap();

    // 42
    debug!("Signatures for Address {}: {:?}", address, signatures.len());

    assert!(signatures.len() > 10, "signatures count is too low");
}


async fn websocket_account_subscribe(
    rpc_url: Url
) {
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
