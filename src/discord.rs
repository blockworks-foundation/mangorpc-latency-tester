use crate::{measure_txs::WatchTxResult, rpcnode_define_checks::Check};
use chrono::SecondsFormat;
use chrono::Utc;
use gethostname::gethostname;
use itertools::Itertools;
use once_cell::sync::OnceCell;
use reqwest::Client;
use serde_json::{json, Value};
use tracing::{error, info, warn};

pub static DISCORD_WEBHOOK_URL: OnceCell<String> = OnceCell::new();

fn create_slot_warning_msg(slot_sent: u64, slot_confirmed: u64) -> Value {
    let status_color = 0xFF0000;

    let content = format!(
        r#"
slot_sent is greater than slot_confirmed.
an rpc node is likely severely lagging...

slot values:
slot_sent: {slot_sent}
slot_confirmed: {slot_confirmed}
        "#
    );

    json! {
        {
            "embeds": [
                {
                    "title": "error: unusual slot values",
                    "description": content,
                    "color": status_color,
                }
            ]
        }
    }
}

fn create_tx_measurement_msg(notify_results: &[WatchTxResult]) -> Value {
    let status_color = 0x0000FF;

    let mut description = String::new();
    description.push_str("```\n");
    for result in notify_results {
        description.push_str(&format!("<{}>", result.label));
        if let Some(tx_result) = &result.result {
            description.push_str(&format!(
                r#"
tx_sig: {}
slot_sent: {}
slot_confirmed: {}
slot_diff: {}"#,
                tx_result.signature,
                tx_result.slot_sent,
                tx_result.slot_confirmed,
                tx_result.slot_confirmed - tx_result.slot_sent,
            ));
        } else if let Some(error) = &result.error {
            description.push_str(&format!(
                r#"
error: {}"#,
                error
            ));
        }

        description.push_str(&format!(
            r#"
"lifetime_avg_slot_diff: {}
lifetime_fails: {}
</{}>

"#,
            result.lifetime_avg, result.lifetime_fails, result.label
        ));
    }
    description.push_str("```\n");

    json! {
        {
            "embeds": [
                {
                    "title": "measure-tx results",
                    "description": description,
                    "username": "RPC Node Check",
                    "color": status_color,
                    "timestamp":  Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
                }
            ]
        }
    }
}

pub fn create_check_alive_discord_message(
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

pub async fn send_webook_discord(url: String, discord_body: Value) {
    let client = Client::new();
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

pub async fn notify_slot_warning(slot_sent: u64, slot_confirmed: u64) {
    if let Some(url) = DISCORD_WEBHOOK_URL.get() {
        let msg = create_slot_warning_msg(slot_sent, slot_confirmed);
        send_webook_discord(url.clone(), msg).await;
    } else {
        warn!("notify_slot_warning: discord notifications disabled");
    }
}

pub async fn notify_tx_measurement(notify_results: &[WatchTxResult]) {
    if let Some(url) = DISCORD_WEBHOOK_URL.get() {
        let msg = create_tx_measurement_msg(notify_results);
        send_webook_discord(url.clone(), msg).await;
    } else {
        warn!("notify_tx_measurement: discord notifications disabled");
    }
}
