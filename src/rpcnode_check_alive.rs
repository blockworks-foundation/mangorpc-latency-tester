mod rpcnode_define_checks;

use gethostname::gethostname;
use itertools::Itertools;
use rpcnode_define_checks::Check;
use rpcnode_define_checks::CheckResult;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::{exit, ExitCode};
use std::time::Duration;
use tokio::task::JoinSet;
use tracing::{debug, error, info, warn};

const TASK_TIMEOUT: Duration = Duration::from_millis(15000);

async fn send_webook_discord(discord_body: Value) {
    let Ok(url) = std::env::var("DISCORD_WEBHOOK") else {
        info!("sending to discord is disabled");
        return;
    };

    let client = reqwest::Client::new();
    let res = client.post(url).json(&discord_body).send().await;
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

    let map_checks_by_name: HashMap<String, Check> = enum_iterator::all::<Check>()
        .map(|check| (format!("{:?}", check), check))
        .collect();

    // comma separated
    let checks_enabled = std::env::var("CHECKS_ENABLED").unwrap();
    debug!("checks_enabled unparsed: {}", checks_enabled);

    let checks_enabled: Vec<Check> = checks_enabled
        .split(',')
        .map(|s| {
            let s = s.trim();

            match map_checks_by_name.get(s) {
                Some(check) => check,
                None => {
                    error!("unknown check: {}", s);
                    exit(1);
                }
            }
        })
        .cloned()
        .collect_vec();

    info!(
        "checks enabled for rpcnode <{}>: {:?}",
        rpcnode_label, checks_enabled
    );

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
                info!(
                    "one more task completed <{:?}>, {}/{} left",
                    check,
                    all_check_tasks.len(),
                    tasks_total
                );
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

    let discord_body = create_discord_message(
        &rpcnode_label,
        checks_enabled,
        &mut tasks_success,
        tasks_timedout,
        success,
    );
    send_webook_discord(discord_body).await;

    if !success {
        warn!(
            "rpcnode <{}> - tasks failed ({}) or timed out ({}) of {} total",
            rpcnode_label, tasks_failed, tasks_timeout, tasks_total
        );
        for check in enum_iterator::all::<Check>() {
            if !tasks_success.contains(&check) {
                warn!("!! did not complet task <{:?}>", check);
            }
        }
        return ExitCode::FAILURE;
    } else {
        info!(
            "rpcnode <{}> - all {} tasks completed: {:?}",
            rpcnode_label, tasks_total, tasks_success
        );
        return ExitCode::SUCCESS;
    }
}

fn create_discord_message(
    rpcnode_label: &str,
    checks_enabled: Vec<Check>,
    tasks_success: &mut [Check],
    tasks_timedout: Vec<Check>,
    success: bool,
) -> Value {
    let result_per_check = enum_iterator::all::<Check>()
        .map(|check| {
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
        })
        .collect_vec();

    let fields = result_per_check;

    let status_color = if success { 0x00FF00 } else { 0xFC4100 };

    let hostname_executed = gethostname();

    let content = if success {
        format!("OK rpc node check for <{}>", rpcnode_label)
    } else {
        let userid_groovie = 933275947124273182u128;
        let role_id_alerts_mangolana = 1100752577307619368u128;
        let mentions = format!("<@{}> <@&{}>", userid_groovie, role_id_alerts_mangolana);
        format!("Failed rpc node check for <{}> {}", rpcnode_label, mentions)
    };

    let body = json! {
        {
            "content": content,
            "description": format!("executed on {}", hostname_executed.to_string_lossy()),
            "username": "RPC Node Check",
            "embeds": [
                {
                    "title": "Check Results",
                    "description": "",
                    "color": status_color,
                    "fields":
                       fields
                    ,
                    "footer": {
                        "text": format!("github: mangorpc-latency-tester, author: groovie")
                    }
                }
            ]
        }
    };
    body
}
