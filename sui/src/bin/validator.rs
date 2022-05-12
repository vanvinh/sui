// Copyright (c) 2022, Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use anyhow::anyhow;
use clap::*;
use multiaddr::Multiaddr;
use narwhal_config::Parameters as ConsensusParameters;
use std::path::PathBuf;
use sui::{
    config::{
        sui_config_dir, GenesisConfig, NetworkConfig, PersistedConfig, CONSENSUS_DB_NAME,
        SUI_NETWORK_CONFIG,
    },
    sui_commands::{genesis, make_server},
};
use sui_types::{
    base_types::{encode_bytes_hex, SuiAddress},
    committee::Committee,
};
use tracing::{error, info};

#[derive(Parser)]
#[clap(
    name = "Sui Validator",
    about = "Validator for Sui Network",
    rename_all = "kebab-case"
)]
struct ValidatorOpt {
    /// The genesis config file location
    #[clap(long)]
    pub genesis_config_path: PathBuf,
    #[clap(long, help = "If set, run genesis even if network.conf already exists")]
    pub force_genesis: bool,

    #[clap(long)]
    pub network_config_path: Option<PathBuf>,

    #[clap(long, help = "Specify host:port to listen on")]
    listen_address: Option<Multiaddr>,
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let config = telemetry_subscribers::TelemetryConfig {
        service_name: "sui".into(),
        enable_tracing: std::env::var("SUI_TRACING_ENABLE").is_ok(),
        json_log_output: std::env::var("SUI_JSON_SPAN_LOGS").is_ok(),
        ..Default::default()
    };

    #[allow(unused)]
    let guard = telemetry_subscribers::init(config);

    let cfg = ValidatorOpt::parse();

    let network_config_path = sui_config_dir()?.join(SUI_NETWORK_CONFIG);

    let network_config = match (network_config_path.exists(), cfg.force_genesis) {
        (true, false) => PersistedConfig::<NetworkConfig>::read(&network_config_path)?,

        // If network.conf is missing, or if --force-genesis is true, we run genesis.
        _ => {
            let genesis_conf: GenesisConfig = PersistedConfig::read(&cfg.genesis_config_path)?;
            let adddress = SuiAddress::from(genesis_conf.key_pair.public_key_bytes());
            let (network_config, _, _) = genesis(genesis_conf, Some(adddress)).await?;
            network_config
        }
    };
    let public_key_bytes = network_config.key_pair.public_key_bytes();
    let address = SuiAddress::from(public_key_bytes);
    // Find the network config for this validator
    let authority = network_config
        .authorities
        .iter()
        .find(|x| SuiAddress::from(&x.public_key) == address)
        .ok_or_else(|| {
            anyhow!(
                "Keypair (pub key: {:?}) in network config is not in the validator committee",
                public_key_bytes,
            )
        })?;

    let listen_address = cfg
        .listen_address
        .unwrap_or_else(|| authority.network_address.clone());

    let consensus_committee = network_config.make_narwhal_committee();

    let consensus_parameters = ConsensusParameters {
        max_header_delay: std::time::Duration::from_millis(5_000),
        max_batch_delay: std::time::Duration::from_millis(5_000),
        ..ConsensusParameters::default()
    };
    let consensus_store_path = sui_config_dir()?
        .join(CONSENSUS_DB_NAME)
        .join(encode_bytes_hex(&authority.public_key));

    info!(
        "Initializing authority {:?} listening on {} (public addr: {})",
        authority.public_key, listen_address, authority.network_address
    );

    // Pass in the newtwork parameters of all authorities
    let net = network_config.get_authority_infos();
    if let Err(e) = make_server(
        authority,
        &network_config.key_pair,
        &Committee::from(&network_config),
        &consensus_committee,
        &consensus_store_path,
        &consensus_parameters,
        Some(net),
    )
    .await?
    .spawn_with_bind_address(listen_address)
    .await
    .unwrap()
    .join()
    .await
    {
        error!("Validator server ended with an error: {e}");
    }

    Ok(())
}
