use anyhow::{bail, Result};
use dotenv::dotenv;
use serde_derive::Deserialize;
use solana_sdk::signature::Keypair;
use std::fs;
use std::iter::zip;
use std::{collections::HashMap, env};
use tracing::error;

pub struct ParsedConfig {
    pub measure_txs: MeasureTxsConfig,
    pub user: Keypair,
}

#[derive(Debug, Deserialize)]
pub struct MeasureTxsConfig {
    pub pubsub_url: String,
    pub rpc_url: String,
    pub helius_url: String,
    pub urls_by_label: HashMap<String, String>,
    pub user_key: String,
}

#[derive(Debug, Deserialize)]
pub struct Config {
    pub measure_txs: MeasureTxsConfig,
}

fn setup_logging() {
    // default to info level logging
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "info");
    }

    let init = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_nanos()
        .try_init();
    if let Err(err) = init {
        error!("env_logger::builder::init: {}", err);
    }
}

pub fn parse_user_key(raw: String) -> Result<Keypair> {
    let byte_strs: Vec<&str> = raw.split(',').collect();
    let bytes: Result<Vec<u8>, _> = byte_strs.iter().map(|s| s.parse::<u8>()).collect();
    let user = Keypair::from_bytes(&bytes?)?;

    Ok(user)
}

pub fn try_parse_toml() -> Result<ParsedConfig> {
    let config_path = fs::read_to_string("config.toml")?;
    let config: Config = toml::from_str(&config_path)?;
    let user = parse_user_key(config.measure_txs.user_key.clone())?;
    let parsed_config = ParsedConfig {
        measure_txs: config.measure_txs,
        user,
    };

    Ok(parsed_config)
}

pub fn try_parse_env() -> Result<ParsedConfig> {
    let pubsub_url = env::var("PUBSUB_URL")?;
    let rpc_url = env::var("RPC_URL")?;
    let helius_url = env::var("HELIUS_URL")?;
    let user_key = env::var("USER_KEY")?;
    let urls_by_label_labels = env::var("URLS_BY_LABEL_LABELS")?;
    let urls_by_label_labels: Vec<&str> = urls_by_label_labels.split(",").collect();
    let urls_by_label_urls = env::var("URLS_BY_LABEL_URLS")?;
    let urls_by_label_urls: Vec<&str> = urls_by_label_urls.split(",").collect();
    if urls_by_label_urls.len() != urls_by_label_labels.len() {
        bail!("urls_by_label_urls len != urls_by_label_labels len");
    }

    let mut urls_by_label: HashMap<String, String> = HashMap::new();
    for (label, url) in zip(urls_by_label_labels, urls_by_label_urls) {
        urls_by_label.insert(String::from(label), String::from(url));
    }

    Ok(ParsedConfig {
        measure_txs: MeasureTxsConfig {
            pubsub_url,
            rpc_url,
            helius_url,
            user_key: user_key.clone(),
            urls_by_label,
        },
        user: parse_user_key(user_key)?,
    })
}

pub fn setup() -> Result<ParsedConfig> {
    dotenv().ok();

    setup_logging();

    if let Ok(config) = try_parse_toml() {
        return Ok(config);
    }

    try_parse_env()
}
