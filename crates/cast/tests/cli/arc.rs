// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Circle Internet Group, Inc.

use alloy_primitives::{Address, B256, U256, address};
use alloy_provider::Provider;
use alloy_rpc_types::{BlockId, BlockTransactions};
use alloy_sol_types::{SolEvent, sol};
use anvil::NodeConfig;
use foundry_test_utils::util::OutputExt;
use std::env;

const ARC_TESTNET_CHAIN_ID: u64 = 5_042_002;
const DEV_PK0: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const DEV_ADDR0: Address = address!("f39fd6e51aad88f6f4ce6ab8827279cfffb92266");
const DEV_ADDR1: Address = address!("70997970c51812dc3a010c7d01b50e0d17dc79c8");
const SYSTEM_ADDRESS: Address = address!("fffffffffffffffffffffffffffffffffffffffe");

sol! {
    event Transfer(address indexed from, address indexed to, uint256 value);
}

fn assert_arc_transfer_log(log: &alloy_rpc_types::Log, from: Address, to: Address, amount: U256) {
    assert_eq!(log.address(), SYSTEM_ADDRESS);
    assert_eq!(log.topics().len(), 3);
    assert_eq!(log.topics()[0], Transfer::SIGNATURE_HASH);
    assert_eq!(log.topics()[1], B256::left_padding_from(from.as_slice()));
    assert_eq!(log.topics()[2], B256::left_padding_from(to.as_slice()));
    assert_eq!(log.data().data.as_ref(), &amount.to_be_bytes::<32>());
}

forgetest_async!(cast_sends_and_replays_arc_transaction, |_prj, cmd| {
    let (_api, handle) =
        anvil::spawn(NodeConfig::test().with_chain_id(Some(ARC_TESTNET_CHAIN_ID))).await;
    let rpc = handle.http_endpoint();

    cmd.cast_fuse()
        .args([
            "send",
            &DEV_ADDR1.to_string(),
            "--value",
            "1",
            "--private-key",
            DEV_PK0,
            "--rpc-url",
            &rpc,
        ])
        .assert_success();

    let provider = handle.http_provider();
    let block = provider.get_block(BlockId::latest()).await.unwrap().unwrap();
    let tx_hash = match &block.transactions {
        BlockTransactions::Hashes(hashes) => hashes[0],
        other => panic!("expected hash transactions, got {other:?}"),
    };
    let receipt = provider.get_transaction_receipt(tx_hash).await.unwrap().unwrap();
    let logs = receipt.inner.inner.logs();
    assert_eq!(logs.len(), 1);
    assert_arc_transfer_log(&logs[0], DEV_ADDR0, DEV_ADDR1, U256::from(1));

    cmd.cast_fuse()
        .args(["run", &tx_hash.to_string(), "--quick", "-vvvvv", "--rpc-url", &rpc])
        .assert_success();
});

forgetest_async!(
    #[ignore = "requires a running Arc localdev RPC; set ARC_LOCALDEV_RPC_URL to override"]
    cast_interoperates_with_arc_localdev,
    |_prj, cmd| {
        let rpc = env::var("ARC_LOCALDEV_RPC_URL")
            .unwrap_or_else(|_| "http://localhost:8545".to_string());
        let private_key =
            env::var("ARC_LOCALDEV_PRIVATE_KEY").unwrap_or_else(|_| DEV_PK0.to_string());
        let recipient =
            env::var("ARC_LOCALDEV_RECIPIENT").unwrap_or_else(|_| DEV_ADDR1.to_string());

        let version = cmd
            .cast_fuse()
            .args(["rpc", "arc_getVersion", "--rpc-url", &rpc])
            .assert_success()
            .get_output()
            .stdout_lossy();
        assert!(version.contains("git_commit") || version.contains("git_version"), "{version}");

        let output = cmd
            .cast_fuse()
            .args([
                "send",
                &recipient,
                "--value",
                "1",
                "--private-key",
                &private_key,
                "--rpc-url",
                &rpc,
                "--json",
            ])
            .assert_success()
            .get_output()
            .stdout_lossy();
        let json: serde_json::Value = serde_json::from_str(&output).unwrap();
        let tx_hash = json["transactionHash"].as_str().unwrap();

        cmd.cast_fuse()
            .args(["run", tx_hash, "--quick", "-vvvvv", "--rpc-url", &rpc])
            .assert_success();
    }
);
