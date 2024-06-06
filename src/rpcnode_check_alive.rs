use std::collections::HashMap;
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
use solana_account_decoder::UiAccountEncoding;
use solana_rpc_client_api::config::{RpcAccountInfoConfig, RpcProgramAccountsConfig};
use solana_rpc_client_api::filter::{Memcmp, RpcFilterType};

type Slot = u64;

const TASK_TIMEOUT: Duration = Duration::from_millis(15000);

enum CheckResult {
    Success(Check),
    Timeout(Check),
}

#[derive(Debug)]
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

    // http://...
    let rpc_addr = std::env::var("RPC_HTTP_ADDR").unwrap();

    // wss://...
    let ws_addr = std::env::var("RPC_WS_ADDR").unwrap();

    // http://...
    let grpc_addr = std::env::var("GRPC_ADDR").unwrap();

    let geyser_grpc_timeouts = GrpcConnectionTimeouts {
        connect_timeout: Duration::from_secs(10),
        request_timeout: Duration::from_secs(10),
        subscribe_timeout: Duration::from_secs(10),
        receive_timeout: Duration::from_secs(10),
    };

    info!("rpcnode_check_alive against {}: (rpc {}, ws {}, gprc {})", rpcnode_label, rpc_addr, ws_addr, grpc_addr);

    let rpc_url = Url::parse(rpc_addr.as_str()).unwrap();
    let rpc_client = Arc::new(RpcClient::new(rpc_url.to_string()));

    let geyser_grpc_config = GrpcSourceConfig::new(grpc_addr.to_string(), None, None, geyser_grpc_timeouts.clone());

    let mut all_check_tasks: JoinSet<CheckResult> = JoinSet::new();

    add_task(Check::Gpa, rpc_gpa(rpc_client.clone()), &mut all_check_tasks);
    add_task(Check::TokenAccouns, rpc_get_token_accounts_by_owner(rpc_client.clone()), &mut all_check_tasks);
    add_task(Check::Gsfa, rpc_get_signatures_for_address(rpc_client.clone()), &mut all_check_tasks);
    add_task(Check::GetAccountInfo, rpc_get_account_info(rpc_client.clone()), &mut all_check_tasks);

    add_task(Check::WebsocketAccount, websocket_account_subscribe(Url::parse(ws_addr.as_str()).unwrap()), &mut all_check_tasks);

    add_task(Check::GeyserAllAccounts, create_geyser_all_accounts_task(geyser_grpc_config.clone()), &mut all_check_tasks);
    add_task(Check::GeyserTokenAccount, create_geyser_token_account_task(geyser_grpc_config.clone()), &mut all_check_tasks);

    info!("all tasks started...");

    let mut tasks_successful = 0;
    let mut tasks_timeout = 0;
    let mut tasks_failed = 0;
    while let Some(res) = all_check_tasks.join_next().await {
        match res {
            Ok(CheckResult::Success(check)) => {
                tasks_successful += 1;
                info!("one more task completed <{:?}>, {} left", check, all_check_tasks.len());
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
        warn!("tasks failed ({}) or timed out ({}) of {} total", tasks_failed, tasks_timeout, tasks_total);
        return ExitCode::SUCCESS;
    } else {
        info!("all {} tasks completed...", tasks_total);
        return ExitCode::FAILURE;
    }
}

fn add_task(check: Check, task: impl Future<Output = ()> + Send + 'static, all_check_tasks: &mut JoinSet<CheckResult>) {
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

async fn rpc_gpa(rpc_client: Arc<RpcClient>)  {

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
