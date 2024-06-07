mod rpcnode_define_checks;

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
use serde_json::{json, Value};
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
use gethostname::gethostname;
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


async fn send_webook_discord(discord_body: Value) {
    let Ok(url) = std::env::var("DISCORD_WEBHOOK") else {
        info!("sending to discord is disabled");
        return;
    };

    let client = reqwest::Client::new();
    let res = client.post(url)
        .json(&discord_body)
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

    rpcnode_define_checks::define_checks(&checks_enabled, &mut all_check_tasks);

    let tasks_total = all_check_tasks.len();
    info!("all {} tasks started...", tasks_total);

    let mut tasks_success = Vec::new();
    let mut tasks_successful = 0;
    let mut tasks_timeout = 0;
    let mut tasks_timedout = Vec::new();
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
                tasks_timedout.push(check);
            }
            Err(_) => {
                tasks_failed += 1;
                warn!("Task execution failed");
            }
        }
    }
    let tasks_total = tasks_successful + tasks_failed + tasks_timeout;
    let success = tasks_failed + tasks_timeout == 0;

    assert!(tasks_total > 0, "no results");


    let discord_body = create_discord_message(&rpcnode_label, checks_enabled, &mut tasks_success, tasks_timedout, success);
    send_webook_discord(discord_body).await;

    if !success {
        warn!("rpcnode <{}> - tasks failed ({}) or timed out ({}) of {} total",
            rpcnode_label, tasks_failed, tasks_timeout, tasks_total);
        for check in enum_iterator::all::<Check>() {
            if !tasks_success.contains(&check) {
                warn!("!! did not complet task <{:?}>", check);
            }
        }
        return ExitCode::FAILURE;
    } else {
        info!("rpcnode <{}> - all {} tasks completed: {:?}", rpcnode_label, tasks_total, tasks_success);
        return ExitCode::SUCCESS;
    }
}

fn create_discord_message(
    rpcnode_label: &str, checks_enabled: Vec<Check>, mut tasks_success: &mut Vec<Check>, mut tasks_timedout: Vec<Check>, success: bool) -> Value {
    let result_per_check = enum_iterator::all::<Check>().map(|check| {
        let name = format!("{:?}", check);
        let disabled = !checks_enabled.contains(&check);
        let timedout = tasks_timedout.contains(&check);
        let success = tasks_success.contains(&check);
        let value = if disabled {
            "disabled"
        } else if timedout {
            "timed out"
        } else if success {
            "OK"
        } else {
            "failed"
        };
        json! {
            {
                "name": name,
                "value": value
            }
        }
    }).collect_vec();

    let fields = result_per_check;

    let status_color = if success {
        0x00FF00
    } else {
        0xFC4100
    };

    let hostname_executed = gethostname();

    let body = json! {
        {
            "content": "Automatic RPC Node check script notification",
            "username": "RPC Node Check",
            "embeds": [
                {
                    "title": rpcnode_label,
                    "description": "",
                    "color": status_color,
                    "fields":
                       fields
                    ,
                    "footer": {
                        "text": format!("by groovie on {}", hostname_executed.to_string_lossy())
                    }
                }
            ]
        }
    };
    body
}

