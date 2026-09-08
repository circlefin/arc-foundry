// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Circle Internet Group, Inc.

use alloy_consensus::{BlobTransactionSidecar, SidecarBuilder, SimpleCoder};
use alloy_network::{ReceiptResponse, TransactionBuilder, TransactionBuilder4844};
use alloy_primitives::{Address, B256, Bytes, U256, address, b256, bytes};
use alloy_provider::Provider;
use alloy_rpc_types::{BlockId, TransactionRequest};
use alloy_serde::WithOtherFields;
use alloy_sol_types::{SolCall, SolEvent, SolValue, sol};
use anvil::{NodeConfig, PrecompileFactory, cmd::NodeArgs, spawn, try_spawn};
use arc_execution_config::{
    chainspec::{LOCAL_DEV, bundled_chainspec_for_chain_id},
    gas_fee::decode_base_fee_from_bytes,
    native_coin_control::compute_is_blocklisted_storage_slot,
};
use clap::Parser;
use foundry_evm::{core::evm::ARC_PROTOCOL_CONFIG_BLOCK_GAS_LIMIT_SLOT, hardfork::ArcHardfork};
use foundry_evm_networks::NetworkConfigs;
use foundry_primitives::FoundryNetwork;
use revm::handler::SYSTEM_ADDRESS;
use std::{
    net::TcpListener,
    process::{Child, Command, Stdio},
    time::Duration,
};

const LOCAL_ARC_CHAIN_ID: u64 = 31_337;
const DEV_ADDR0: Address = address!("f39fd6e51aad88f6f4ce6ab8827279cfffb92266");
const DEV_ADDR1: Address = address!("70997970c51812dc3a010c7d01b50e0d17dc79c8");
const NATIVE_COIN_CONTROL: Address = address!("1800000000000000000000000000000000000001");
const SYSTEM_ACCOUNTING: Address = address!("1800000000000000000000000000000000000002");
const PROTOCOL_CONFIG: Address = address!("3600000000000000000000000000000000000001");
const PQ_PRECOMPILE: Address = address!("1800000000000000000000000000000000000004");
const DENYLIST_CONTRACT: Address = address!("36059b615370eb999e8ec0c9401835b407834221");
const DEFAULT_DENYLIST_ERC7201_BASE_SLOT: B256 =
    b256!("1d7e1388d3ae56f3d9c18b1ce8d2b3b1a238a0edf682d2053af5d8a1d2f12f00");
const PUSH20: u8 = 0x73;
const SELFDESTRUCT: u8 = 0xff;

sol! {
    event Transfer(address indexed from, address indexed to, uint256 value);

    interface ISystemAccounting {
        function getGasValues(uint64 blockNumber)
            external
            view
            returns (uint64 gasUsed, uint64 gasUsedSmoothed, uint64 nextBaseFee);
    }

    interface IProtocolConfig {
        function updateBlockGasLimit(uint256 newBlockGasLimit) external;
    }
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn unused_tcp_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port()
}

fn arc_node_config_with_hardfork(hardfork: ArcHardfork) -> NodeConfig {
    // The Arc EVM factory reads the selection from `networks`, so it has to be set there too;
    // `with_hardfork` only records it on `NodeConfig::hardfork`.
    NodeConfig::test()
        .with_networks(NetworkConfigs::with_arc().with_arc_hardfork(hardfork))
        .with_hardfork(Some(hardfork.into()))
}

fn arc_node_config() -> NodeConfig {
    NodeConfig::test().with_networks(NetworkConfigs::with_arc())
}

fn compute_denylist_storage_slot(address: Address, base_slot: B256) -> B256 {
    alloy_primitives::keccak256((address, base_slot).abi_encode())
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_pending_block_uses_protocol_base_fee_lifecycle_without_mining() {
    let (api, handle) = spawn(arc_node_config()).await;
    let provider = handle.http_provider();

    let pending = provider.get_block(BlockId::pending()).await.unwrap().unwrap();
    let pending_next_base_fee = decode_base_fee_from_bytes(&pending.header.extra_data).unwrap();
    assert_eq!(provider.get_block_number().await.unwrap(), 0);

    api.try_mine_one().await.unwrap();
    let mined = provider.get_block(BlockId::number(1)).await.unwrap().unwrap();
    let mined_next_base_fee = decode_base_fee_from_bytes(&mined.header.extra_data).unwrap();
    assert_eq!(pending_next_base_fee, mined_next_base_fee);
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_pending_block_query_errors_instead_of_panicking_on_lifecycle_failure() {
    let (api, handle) = spawn(arc_node_config()).await;
    let provider = handle.http_provider();
    let original_coinbase = api.backend.coinbase();

    // A zero beneficiary makes the Arc block lifecycle fail. Building the pending block runs that
    // lifecycle, so the query must surface an RPC error instead of panicking the whole node.
    api.anvil_set_coinbase(Address::ZERO).await.unwrap();
    let err = provider.get_block(BlockId::pending()).await.unwrap_err().to_string();
    assert!(err.to_ascii_lowercase().contains("beneficiary"), "{err}");

    // The node is still alive and keeps serving other requests.
    assert_eq!(provider.get_block_number().await.unwrap(), 0);

    // Restoring a valid beneficiary lets pending queries and mining work again.
    api.anvil_set_coinbase(original_coinbase).await.unwrap();
    assert!(provider.get_block(BlockId::pending()).await.unwrap().is_some());
    api.try_mine_one().await.unwrap();
    assert_eq!(provider.get_block_number().await.unwrap(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_evm_set_block_gas_limit_rejects_out_of_bounds() {
    let (api, handle) = spawn(arc_node_config()).await;
    let provider = handle.http_provider();

    // localdev's ProtocolConfig bounds are [1_000_000, 1_000_000_000]. 2B is above max, so it must
    // be rejected up front: `expected_gas_limit` would clamp/fall back to the default, leaving the
    // value we wrote diverging from what the block executor computes.
    let err =
        api.evm_set_block_gas_limit(U256::from(2_000_000_000u64)).await.unwrap_err().to_string();
    assert!(err.contains("out of range"), "{err}");

    // The node keeps producing blocks normally after the rejected call.
    api.try_mine_one().await.unwrap();
    assert_eq!(provider.get_block_number().await.unwrap(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_follows_protocol_config_block_gas_limit_governance_change() {
    // The real governance mechanism: the ProtocolConfig controller sends a transaction that changes
    // blockGasLimit. Anvil must re-derive the block gas limit from ProtocolConfig every block, or
    // the block after the change would carry the stale limit and the block executor would reject
    // Anvil's own block, stalling production. localdev's controller is default dev account #8.
    let (api, handle) = spawn(arc_node_config()).await;
    let provider = handle.http_provider();
    let controller = address!("23618e81e3f5cdf7f54c3d65f7fbc0abf5b21e8f");

    let new_limit = 60_000_000u64;
    let input =
        IProtocolConfig::updateBlockGasLimitCall { newBlockGasLimit: U256::from(new_limit) }
            .abi_encode();
    let receipt = provider
        .send_transaction(WithOtherFields::new(
            TransactionRequest::default().from(controller).to(PROTOCOL_CONFIG).input(input.into()),
        ))
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();
    assert!(receipt.status(), "updateBlockGasLimit reverted");

    // The block carrying the governance tx keeps the pre-change limit: the executor reads
    // ProtocolConfig before the block's transactions run.
    let gov_block = provider.get_block(BlockId::latest()).await.unwrap().unwrap();
    assert_eq!(gov_block.header.gas_limit, 30_000_000);

    // The next block adopts the new limit and production continues.
    api.mine_one().await;
    let next_block = provider.get_block(BlockId::latest()).await.unwrap().unwrap();
    assert_eq!(next_block.header.gas_limit, new_limit);
    assert_eq!(next_block.header.number, gov_block.header.number + 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_governance_lowering_gas_limit_refreshes_pool_and_pending_immediately() {
    // After the block carrying a governance change commits, the paths that build a candidate child
    // block (pool admission, pending query) derive the limit from current state — so they reflect
    // ProtocolConfig immediately, without a further mine and without a stale-cache mismatch.
    let (_api, handle) = spawn(arc_node_config()).await;
    let provider = handle.http_provider();
    let wallets = handle.dev_wallets().collect::<Vec<_>>();
    let controller = address!("23618e81e3f5cdf7f54c3d65f7fbc0abf5b21e8f");
    let sender = wallets[0].address();

    // Governance lowers the block gas limit from the 30M default to 20M.
    let lowered = 20_000_000u64;
    let input = IProtocolConfig::updateBlockGasLimitCall { newBlockGasLimit: U256::from(lowered) }
        .abi_encode();
    let receipt = provider
        .send_transaction(WithOtherFields::new(
            TransactionRequest::default().from(controller).to(PROTOCOL_CONFIG).input(input.into()),
        ))
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();
    assert!(receipt.status(), "updateBlockGasLimit reverted");

    // The pending block (a candidate child) derives its limit from current state: 20M immediately.
    let pending = provider.get_block(BlockId::pending()).await.unwrap().unwrap();
    assert_eq!(pending.header.gas_limit, lowered);

    // A 25M-gas tx is between the new (20M) and old (30M) limit: pool admission derives against
    // current state and rejects it, instead of accepting it against a stale 30M cache.
    let too_big = TransactionRequest::default().from(sender).to(sender).gas_limit(25_000_000u64);
    let err = provider
        .send_transaction(WithOtherFields::new(too_big))
        .await
        .expect_err("25M-gas tx must be rejected against the 20M limit");
    assert!(err.to_string().to_lowercase().contains("gas"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_startup_rejects_gas_limit_not_matching_protocol_config() {
    // Arc's block gas limit is protocol-controlled. A `--gas-limit` that disagrees with the block
    // gas limit derived from the final startup state (genesis ProtocolConfig = 30M) is rejected at
    // startup; the matching value starts normally. Checked on every selectable hardfork.
    for hardfork in [ArcHardfork::Zero6, ArcHardfork::Zero7, ArcHardfork::Zero8] {
        let err =
            try_spawn(arc_node_config_with_hardfork(hardfork).with_gas_limit(Some(50_000_000)))
                .await
                .err()
                .unwrap_or_else(|| {
                    panic!("{hardfork:?}: startup must reject a non-protocol --gas-limit")
                });
        assert!(err.to_string().contains("does not match"), "{hardfork:?}: {err}");

        assert!(
            try_spawn(arc_node_config_with_hardfork(hardfork).with_gas_limit(Some(30_000_000)))
                .await
                .is_ok(),
            "{hardfork:?}: the protocol gas limit must start"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_startup_rejects_disable_block_gas_limit() {
    let err = try_spawn(arc_node_config().disable_block_gas_limit(true))
        .await
        .err()
        .expect("startup must reject --disable-block-gas-limit on Arc");
    assert!(err.to_string().contains("disable-block-gas-limit"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_call_uses_recorded_header_for_latest_and_historical_but_derives_for_pending() {
    // Historical/latest execution must use the gas limit recorded in that block's header; only
    // pending (a candidate child) derives the current ProtocolConfig value. A probe contract
    // returns `block.gaslimit` (GASLIMIT; PUSH1 0; MSTORE; PUSH1 32; PUSH1 0; RETURN).
    let (api, handle) = spawn(arc_node_config()).await;
    let provider = handle.http_provider();
    let controller = address!("23618e81e3f5cdf7f54c3d65f7fbc0abf5b21e8f");
    let probe = address!("00000000000000000000000000000000000000aa");
    api.anvil_set_code(probe, bytes!("4560005260206000f3")).await.unwrap();

    // Block 1 mined at the 30M default.
    api.mine_one().await;

    // Governance lowers to 20M; the block carrying the tx still uses the pre-change 30M.
    let input =
        IProtocolConfig::updateBlockGasLimitCall { newBlockGasLimit: U256::from(20_000_000u64) }
            .abi_encode();
    provider
        .send_transaction(WithOtherFields::new(
            TransactionRequest::default().from(controller).to(PROTOCOL_CONFIG).input(input.into()),
        ))
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();

    let gas_limit_at = |id: BlockId| {
        let provider = provider.clone();
        async move {
            let out = provider
                .call(WithOtherFields::new(TransactionRequest::default().to(probe)))
                .block(id)
                .await
                .unwrap();
            U256::from_be_slice(&out).to::<u64>()
        }
    };

    // Historical block 1 and latest (the governance block, header 30M) read their recorded header;
    // pending derives the new 20M.
    assert_eq!(gas_limit_at(BlockId::number(1)).await, 30_000_000);
    assert_eq!(gas_limit_at(BlockId::latest()).await, 30_000_000);
    assert_eq!(gas_limit_at(BlockId::pending()).await, 20_000_000);
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_pending_and_mining_follow_raw_protocol_config_storage_write() {
    // Pull-based: there is no cache to refresh. A raw storage write to the ProtocolConfig
    // blockGasLimit slot is followed immediately by the pending block (which derives) and by the
    // next mined block — no mine and no refresh hook in between.
    let (api, handle) = spawn(arc_node_config()).await;
    let provider = handle.http_provider();

    set_storage_word(
        &api,
        PROTOCOL_CONFIG,
        ARC_PROTOCOL_CONFIG_BLOCK_GAS_LIMIT_SLOT,
        U256::from(20_000_000u64),
    )
    .await;

    let pending = provider.get_block(BlockId::pending()).await.unwrap().unwrap();
    assert_eq!(pending.header.gas_limit, 20_000_000);

    api.mine_one().await;
    let latest = provider.get_block(BlockId::latest()).await.unwrap().unwrap();
    assert_eq!(latest.header.gas_limit, 20_000_000);
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_concurrent_pending_during_mining_never_sees_torn_state() {
    // Concurrency smoke test: hammer pending queries while repeatedly mining. Every next-block
    // consumer derives its limit under its own db snapshot, so a pending query can only linearize
    // to a consistent state — never a torn (new state + stale limit) view that would surface as
    // an RPC error or an out-of-bounds limit.
    let (api, handle) = spawn(arc_node_config()).await;
    let provider = handle.http_provider();

    let mining = async {
        for _ in 0..12u64 {
            api.mine_one().await;
        }
    };
    let querying = async {
        for _ in 0..80u64 {
            let block = provider.get_block(BlockId::pending()).await.unwrap().unwrap();
            assert!(
                (1_000_000..=1_000_000_000).contains(&block.header.gas_limit),
                "pending gas limit {} out of ProtocolConfig bounds",
                block.header.gas_limit
            );
        }
    };
    tokio::join!(mining, querying);
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_evm_set_block_gas_limit_updates_protocol_config_and_keeps_producing() {
    let (api, handle) = spawn(arc_node_config()).await;
    let provider = handle.http_provider();

    // 50M is inside localdev's [1M, 1B] band. On Arc this writes the ProtocolConfig storage slot
    // (the source the block executor reads), not just `block_env`, so the executor's expected limit
    // moves with it and the node keeps producing instead of rejecting its own blocks.
    let new_limit = 50_000_000u64;
    assert!(api.evm_set_block_gas_limit(U256::from(new_limit)).await.unwrap());

    // Each mined block must carry the new limit and the height must advance. A wrong storage slot
    // would leave ProtocolConfig at the default (30M) while `block_env` is 50M, so the executor
    // would reject block 1 and the height would stall — this asserts the slot is the one the
    // contract actually reads, and that the per-block re-derivation keeps following it.
    for expected_number in 1..=2u64 {
        api.try_mine_one().await.unwrap();
        assert_eq!(provider.get_block_number().await.unwrap(), expected_number);
        let block = provider.get_block(BlockId::latest()).await.unwrap().unwrap();
        assert_eq!(block.header.gas_limit, new_limit);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_prices_consecutive_transactions_from_the_advertised_base_fee() {
    // Regression: Arc used to start at Ethereum's initial base fee, below the protocol floor. The
    // first mined block clamped up to the floor, so every later transaction priced from the
    // preceding header's (stale, lower) base fee was rejected and no further block could be mined.
    let (api, handle) = spawn(arc_node_config()).await;
    let provider = handle.http_provider();
    let wallets = handle.dev_wallets().collect::<Vec<_>>();
    let sender = wallets[0].address();
    let recipient = wallets[1].address();

    for i in 1..=3u64 {
        let receipt = provider
            .send_transaction(WithOtherFields::new(
                TransactionRequest::default().from(sender).to(recipient).value(U256::from(1)),
            ))
            .await
            .unwrap_or_else(|err| panic!("transaction {i} was rejected: {err}"))
            .get_receipt()
            .await
            .unwrap();
        assert!(receipt.status(), "transaction {i} reverted");
        assert_eq!(provider.get_block_number().await.unwrap(), i);

        // Each mined header must advertise a base fee that the block after it still accepts.
        // A client pricing `maxFeePerGas` off this header (the EIP-1559 norm, and what `cast`
        // does) would otherwise be rejected for underpaying, with no later block to correct it.
        let block = provider.get_block(BlockId::number(i)).await.unwrap().unwrap();
        let advertised = block.header.base_fee_per_gas.unwrap();
        let required = decode_base_fee_from_bytes(&block.header.extra_data).unwrap();
        assert!(
            advertised.saturating_mul(2) >= required,
            "block {i} advertises base fee {advertised} but block {} requires {required}; \
             a client pricing from this header underpays",
            i + 1,
        );
    }

    let latest = provider.get_block(BlockId::latest()).await.unwrap().unwrap();
    assert_eq!(
        api.backend.base_fee(),
        decode_base_fee_from_bytes(&latest.header.extra_data).unwrap()
    );
}

/// Regression for the pool-admission base-fee fix (`validate_pool_transaction`): admission must
/// price a candidate transaction against the *next* block's base fee — what mining enforces — not
/// the parent header's. When the base fee is falling, a `maxFeePerGas` sitting between the two
/// (here exactly the next block's fee, with no client buffer) was wrongly rejected at admission
/// before the fix, so the transaction could never be mined. The sibling `arc_prices_*` test only
/// exercises the 2x-buffered client path, which never reaches this boundary.
#[tokio::test(flavor = "multi_thread")]
async fn arc_admits_transaction_priced_exactly_at_the_next_block_base_fee() {
    // Start above the protocol floor so the base fee can fall — at the floor every block clamps to
    // the same value and the parent/next window never opens.
    let (_api, handle) = spawn(arc_node_config().with_base_fee(Some(80_000_000_000u64))).await;
    let provider = handle.http_provider();
    let wallets = handle.dev_wallets().collect::<Vec<_>>();
    let from = wallets[0].address();
    let to = wallets[1].address();

    // Mine one block. A value transfer nudges the EMA so the next block's base fee falls below this
    // header's, opening the window the fix addresses.
    provider
        .send_transaction(WithOtherFields::new(
            TransactionRequest::default().from(from).to(to).value(U256::from(1)),
        ))
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();
    let head = provider.get_block_number().await.unwrap();

    let head_block = provider.get_block(BlockId::number(head)).await.unwrap().unwrap();
    let parent_base_fee = head_block.header.base_fee_per_gas.unwrap();
    // The next block enforces exactly this fee (Arc carries it in the parent's extra_data).
    let next_base_fee = decode_base_fee_from_bytes(&head_block.header.extra_data).unwrap();
    assert!(
        next_base_fee < parent_base_fee,
        "test needs a falling base fee to be meaningful: parent={parent_base_fee}, next={next_base_fee}"
    );

    // Price exactly at the next block's base fee: no priority tip, no client buffer. This is below
    // the parent's base fee, so pre-fix admission (which used the parent's) rejected it.
    let receipt = provider
        .send_transaction(WithOtherFields::new(
            TransactionRequest::default()
                .from(from)
                .to(to)
                .value(U256::from(1))
                .max_fee_per_gas(next_base_fee as u128)
                .max_priority_fee_per_gas(0),
        ))
        .await
        .expect("a transaction priced at the next block base fee must be admitted")
        .get_receipt()
        .await
        .unwrap();
    assert!(receipt.status(), "transaction reverted");
    assert_eq!(provider.get_block_number().await.unwrap(), head + 1, "transaction was not mined");
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_rejects_manual_base_fee_override_and_keeps_following_the_carrier() {
    // On Arc the base fee is derived from the parent block's carrier; a manual override is rejected
    // (both anvil_ and hardhat_ names share one handler), and mining keeps following the carrier.
    let (api, handle) = spawn(arc_node_config()).await;
    let provider = handle.http_provider();

    // Block 1's parent is genesis, which the Arc base-fee rule exempts. Mine it so block 2 has a
    // non-genesis parent whose carrier must be obeyed.
    api.mine_one().await;
    let block1 = provider.get_block(BlockId::number(1)).await.unwrap().unwrap();
    let carrier = decode_base_fee_from_bytes(&block1.header.extra_data)
        .expect("block 1 must carry the next base fee");

    let err = api
        .anvil_set_next_block_base_fee_per_gas(U256::from(1))
        .await
        .expect_err("Arc must reject a manual base fee override");
    assert!(err.to_string().contains("cannot be overridden on Arc"), "{err}");

    // Block 2 (parent = block 1) adopts block 1's carrier, not the rejected override.
    api.mine_one().await;
    let block2 = provider.get_block(BlockId::number(2)).await.unwrap().unwrap();
    assert_eq!(block2.header.base_fee_per_gas, Some(carrier));
}

#[tokio::test(flavor = "multi_thread")]
async fn non_arc_still_allows_base_fee_override() {
    // Regression: the Arc guard must not leak to other networks. A default (Ethereum) node still
    // honors anvil_setNextBlockBaseFeePerGas.
    let (api, handle) = spawn(NodeConfig::test()).await;
    let provider = handle.http_provider();

    let target = 42_000_000_000u64;
    api.anvil_set_next_block_base_fee_per_gas(U256::from(target)).await.unwrap();
    api.mine_one().await;

    let block = provider.get_block(BlockId::latest()).await.unwrap().unwrap();
    assert_eq!(block.header.base_fee_per_gas, Some(target));
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_in_memory_reset_restores_the_protocol_base_fee_floor() {
    // A fresh Arc node starts at the ProtocolConfig base-fee floor (above Ethereum's 1 gwei
    // INITIAL_BASE_FEE). An in-memory reset must restore that floor, not the Ethereum initial fee,
    // or the first block after reset starts below the floor and prices out later transactions.
    let (api, _handle) = spawn(arc_node_config()).await;

    let floor = api.backend.base_fee();
    assert_ne!(floor, 1_000_000_000, "Arc floor must differ from Ethereum's INITIAL_BASE_FEE");

    for _ in 0..3u64 {
        api.mine_one().await;
    }
    api.backend.reset_to_in_mem().await.unwrap();

    assert_eq!(api.backend.base_fee(), floor, "reset must restore the Arc protocol base-fee floor");
}

#[tokio::test(flavor = "multi_thread")]
async fn fork_url_auto_detects_arc_from_the_remote_chain_id() {
    // Use a local Anvil origin so this covers the real fork setup path without a public RPC
    // dependency. The fork starts with the default network config; its Arc mode must come from
    // the origin's `eth_chainId` response.
    let (_origin_api, origin_handle) =
        spawn(NodeConfig::test().with_chain_id(Some(5_042_002u64))).await;
    let (fork_api, _fork_handle) =
        spawn(NodeConfig::test().with_eth_rpc_url(Some(origin_handle.http_endpoint()))).await;

    assert!(fork_api.backend.is_arc());
    // Anvil does not adopt the forked block's miner, so without the chain contributing its genesis
    // coinbase a forked Arc node would start on a zero beneficiary and never produce a block.
    let expected = bundled_chainspec_for_chain_id(5_042_002)
        .expect("testnet is a bundled Arc chain")
        .inner
        .genesis
        .coinbase;
    assert_eq!(fork_api.backend.coinbase(), expected);
    let before = fork_api.backend.best_number();
    fork_api.try_mine_one().await.unwrap();
    assert_eq!(fork_api.backend.best_number(), before + 1);
}

/// The beneficiary is resolved in priority order — a `--init` genesis file over the forked chain's
/// genesis over the local chainspec — which the ordering in `setup` delivers by applying the
/// genesis file last.
#[tokio::test(flavor = "multi_thread")]
async fn arc_genesis_file_overrides_the_forked_chains_beneficiary() {
    let (_origin_api, origin_handle) =
        spawn(NodeConfig::test().with_chain_id(Some(5_042_002u64))).await;

    // Matches no bundled Arc chainspec, so only the genesis file can be the source.
    let chosen = address!("0x00000000000000000000000000000000deadbeef");
    let mut genesis = LOCAL_DEV.inner.genesis.clone();
    genesis.coinbase = chosen;

    let (fork_api, _fork_handle) = spawn(
        NodeConfig::test()
            .with_eth_rpc_url(Some(origin_handle.http_endpoint()))
            .with_genesis(Some(genesis)),
    )
    .await;

    assert!(fork_api.backend.is_arc());
    assert_eq!(fork_api.backend.coinbase(), chosen, "the genesis file must win over the chain");
}

async fn set_storage_word(
    api: &anvil::eth::api::EthApi<FoundryNetwork>,
    address: Address,
    slot: B256,
    value: U256,
) {
    api.anvil_set_storage_at(address, U256::from_be_bytes(slot.0), B256::from(value))
        .await
        .unwrap();
}

async fn mark_native_coin_blocklisted(
    api: &anvil::eth::api::EthApi<FoundryNetwork>,
    address: Address,
) {
    set_storage_word(
        api,
        NATIVE_COIN_CONTROL,
        compute_is_blocklisted_storage_slot(address),
        U256::from(1),
    )
    .await;
}

fn assert_arc_eip7708_transfer_log(
    log: &alloy_rpc_types::Log,
    from: Address,
    to: Address,
    amount: U256,
) {
    assert_eq!(log.address(), SYSTEM_ADDRESS);
    assert_eq!(log.topics().len(), 3);
    assert_eq!(log.topics()[0], Transfer::SIGNATURE_HASH);
    assert_eq!(log.topics()[1], B256::left_padding_from(from.as_slice()));
    assert_eq!(log.topics()[2], B256::left_padding_from(to.as_slice()));
    assert_eq!(log.data().data.as_ref(), &amount.to_be_bytes::<32>());
}

fn decode_u64_word(output: &Bytes, word: usize) -> u64 {
    let start = word * 32;
    let end = start + 32;
    U256::from_be_slice(&output[start..end]).to()
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_anvil_binary_cli_serves_arc_rpc_and_mines_transfer() {
    let port = unused_tcp_port();
    let port = port.to_string();
    let rpc = format!("http://127.0.0.1:{port}");
    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_anvil"))
            .args([
                "--host",
                "127.0.0.1",
                "--port",
                &port,
                "--arc",
                "--hardfork",
                "arc:zero6",
                "--chain-id",
                &LOCAL_ARC_CHAIN_ID.to_string(),
                "--silent",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let provider = crate::utils::http_provider(&rpc);

    let mut ready = false;
    for _ in 0..100 {
        if let Some(status) = child.0.try_wait().unwrap() {
            panic!("anvil exited before RPC became ready: {status}");
        }
        if matches!(provider.get_chain_id().await, Ok(id) if id == LOCAL_ARC_CHAIN_ID) {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(ready, "anvil RPC did not become ready at {rpc}");
    assert_eq!(provider.get_chain_id().await.unwrap(), LOCAL_ARC_CHAIN_ID);

    let amount = U256::from(1);
    let tx = TransactionRequest::default().from(DEV_ADDR0).to(DEV_ADDR1).value(amount);
    let receipt = provider
        .send_transaction(WithOtherFields::new(tx))
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();

    assert!(receipt.status());
    let logs = receipt.inner.inner.logs();
    assert_eq!(logs.len(), 1);
    assert_arc_eip7708_transfer_log(&logs[0], DEV_ADDR0, DEV_ADDR1, amount);
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_transfer_emits_eip7708_log_rewards_full_fee_and_updates_base_fee_carrier() {
    let (api, handle) = spawn(arc_node_config()).await;
    let provider = handle.http_provider();
    let wallets = handle.dev_wallets().collect::<Vec<_>>();
    let from = wallets[0].address();
    let to = wallets[1].address();
    let beneficiary = address!("ba5e000000000000000000000000000000000001");
    let amount = U256::from(1);

    api.anvil_set_coinbase(beneficiary).await.unwrap();
    let beneficiary_before = provider.get_balance(beneficiary).await.unwrap();

    let tx = TransactionRequest::default().from(from).to(to).value(amount);
    let receipt = provider
        .send_transaction(WithOtherFields::new(tx))
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();

    assert!(receipt.status());
    let logs = receipt.inner.inner.logs();
    assert_eq!(logs.len(), 1);
    assert_arc_eip7708_transfer_log(&logs[0], from, to, amount);

    let beneficiary_after = provider.get_balance(beneficiary).await.unwrap();
    let paid_fee = U256::from(receipt.gas_used) * U256::from(receipt.effective_gas_price);
    assert_eq!(beneficiary_after - beneficiary_before, paid_fee);

    let block = provider.get_block(BlockId::latest()).await.unwrap().unwrap();
    let next_base_fee = decode_base_fee_from_bytes(&block.header.extra_data)
        .expect("Arc block extra_data should carry the next base fee");

    let input =
        ISystemAccounting::getGasValuesCall { blockNumber: block.header.number }.abi_encode();
    let output = provider
        .call(TransactionRequest::default().to(SYSTEM_ACCOUNTING).input(input.into()).into())
        .await
        .unwrap();
    assert_eq!(decode_u64_word(&output, 0), receipt.gas_used);
    assert_eq!(decode_u64_word(&output, 2), next_base_fee);
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_uses_parent_extra_data_as_the_next_block_base_fee() {
    let (api, handle) = spawn(arc_node_config()).await;
    let provider = handle.http_provider();
    let wallets = handle.dev_wallets().collect::<Vec<_>>();

    let _receipt = provider
        .send_transaction(WithOtherFields::new(
            TransactionRequest::default().from(wallets[0].address()).to(wallets[1].address()),
        ))
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();
    let parent = provider.get_block(BlockId::latest()).await.unwrap().unwrap();
    let expected_base_fee = decode_base_fee_from_bytes(&parent.header.extra_data)
        .expect("Arc parent block must carry its child's base fee");
    assert_eq!(api.backend.base_fee(), expected_base_fee);

    api.mine_one().await;
    let child = provider.get_block(BlockId::latest()).await.unwrap().unwrap();

    assert_eq!(child.header.base_fee_per_gas, Some(expected_base_fee));
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_native_coin_control_blocklist_rejects_value_recipient_but_not_zero_value() {
    let (api, handle) = spawn(arc_node_config()).await;
    let provider = handle.http_provider();
    let wallets = handle.dev_wallets().collect::<Vec<_>>();
    let from = wallets[0].address();
    let blocked = wallets[1].address();

    mark_native_coin_blocklisted(&api, blocked).await;

    let value_tx = TransactionRequest::default().from(from).to(blocked).value(U256::from(1));
    let err = provider.send_transaction(WithOtherFields::new(value_tx)).await.unwrap_err();
    let err = err.to_string();
    assert!(err.contains("blocklisted by NativeCoinControl"), "{err}");

    let zero_value_tx = TransactionRequest::default().from(from).to(blocked).value(U256::ZERO);
    let receipt = provider
        .send_transaction(WithOtherFields::new(zero_value_tx))
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();
    assert!(receipt.status());
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_native_coin_control_blocklist_rejects_sender_even_for_zero_value() {
    let (api, handle) = spawn(arc_node_config()).await;
    let provider = handle.http_provider();
    let wallets = handle.dev_wallets().collect::<Vec<_>>();
    let blocked_sender = wallets[0].address();
    let to = wallets[1].address();

    mark_native_coin_blocklisted(&api, blocked_sender).await;

    let tx = TransactionRequest::default().from(blocked_sender).to(to).value(U256::ZERO);
    let err = provider.send_transaction(WithOtherFields::new(tx)).await.unwrap_err();
    let err = err.to_string();
    assert!(err.contains("blocklisted by NativeCoinControl"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_does_not_enforce_addresses_denylist() {
    let args = NodeArgs::parse_from(["anvil", "--port", "0", "--arc", "--hardfork", "arc:zero6"]);
    let (api, handle) = spawn(args.into_node_config().unwrap().set_silent(true)).await;
    let provider = handle.http_provider();
    let wallets = handle.dev_wallets().collect::<Vec<_>>();
    let recipient = wallets[1].address();

    let slot = compute_denylist_storage_slot(recipient, DEFAULT_DENYLIST_ERC7201_BASE_SLOT);
    set_storage_word(&api, DENYLIST_CONTRACT, slot, U256::from(1)).await;

    let tx =
        TransactionRequest::default().from(wallets[0].address()).to(recipient).value(U256::from(1));
    let receipt = provider
        .send_transaction(WithOtherFields::new(tx))
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();
    assert!(receipt.status());
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_rejects_value_transfer_to_zero_address() {
    let (_api, handle) = spawn(arc_node_config()).await;
    let provider = handle.http_provider();
    let from = handle.dev_wallets().next().unwrap().address();

    let tx = TransactionRequest::default().from(from).to(Address::ZERO).value(U256::from(1));
    let receipt = provider
        .send_transaction(WithOtherFields::new(tx))
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();

    assert!(!receipt.status());
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_create_value_transfer_emits_eip7708_log() {
    let (_api, handle) = spawn(arc_node_config()).await;
    let provider = handle.http_provider();
    let from = handle.dev_wallets().next().unwrap().address();
    let amount = U256::from(1);

    let tx = TransactionRequest::default()
        .from(from)
        .into_create()
        .value(amount)
        .input(bytes!("00").into());
    let receipt = provider
        .send_transaction(WithOtherFields::new(tx))
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();

    assert!(receipt.status());
    let created = receipt.contract_address.unwrap();
    let logs = receipt.inner.inner.logs();
    assert_eq!(logs.len(), 1);
    assert_arc_eip7708_transfer_log(&logs[0], from, created, amount);
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_selfdestruct_balance_transfer_emits_eip7708_log() {
    let (api, handle) = spawn(arc_node_config()).await;
    let provider = handle.http_provider();
    let caller = handle.dev_wallets().next().unwrap().address();
    let selfdestruct_contract = address!("de5d000000000000000000000000000000000001");
    let recipient = address!("de5d000000000000000000000000000000000002");
    let amount = U256::from(123);

    // Runtime bytecode: PUSH20 <recipient>; SELFDESTRUCT.
    let mut code = Vec::with_capacity(22);
    code.push(PUSH20);
    code.extend_from_slice(recipient.as_slice());
    code.push(SELFDESTRUCT);
    api.anvil_set_code(selfdestruct_contract, code.into()).await.unwrap();
    api.anvil_set_balance(selfdestruct_contract, amount).await.unwrap();

    let receipt = provider
        .send_transaction(WithOtherFields::new(
            TransactionRequest::default().from(caller).to(selfdestruct_contract),
        ))
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();

    assert!(receipt.status());
    let logs = receipt.inner.inner.logs();
    assert_eq!(logs.len(), 1);
    assert_arc_eip7708_transfer_log(&logs[0], selfdestruct_contract, recipient, amount);
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_zero6_uses_eip7708_native_coin_transfer_log() {
    let (_api, handle) = spawn(arc_node_config_with_hardfork(ArcHardfork::Zero6)).await;
    let provider = handle.http_provider();
    let wallets = handle.dev_wallets().collect::<Vec<_>>();
    let from = wallets[0].address();
    let to = wallets[1].address();
    let amount = U256::from(1);

    let tx = TransactionRequest::default().from(from).to(to).value(amount);
    let receipt = provider
        .send_transaction(WithOtherFields::new(tx))
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();

    assert!(receipt.status());
    let logs = receipt.inner.inner.logs();
    assert_eq!(logs.len(), 1);
    assert_arc_eip7708_transfer_log(&logs[0], from, to, amount);
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_enforces_protocol_block_gas_limit_for_all_selectable_hardforks() {
    // 12345 is below the [1M, 1B] band on every selectable hardfork, so it is rejected out of hand.
    for hardfork in [ArcHardfork::Zero6, ArcHardfork::Zero7, ArcHardfork::Zero8] {
        let (api, _) = spawn(arc_node_config_with_hardfork(hardfork)).await;
        let err = api.evm_set_block_gas_limit(U256::from(12345)).await.unwrap_err().to_string();
        assert!(err.contains("out of range"), "{hardfork:?}: {err}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_rejects_blob_transactions_at_submission() {
    let (_api, handle) = spawn(arc_node_config()).await;
    let provider = handle.http_provider();
    let wallets = handle.dev_wallets().collect::<Vec<_>>();
    let from = wallets[0].address();
    let to = wallets[1].address();

    let eip1559 = provider.estimate_eip1559_fees().await.unwrap();
    let sidecar: BlobTransactionSidecar =
        SidecarBuilder::<SimpleCoder>::from_slice(b"Arc rejects blobs").build().unwrap();
    let tx = TransactionRequest::default()
        .from(from)
        .to(to)
        .with_blob_sidecar(sidecar.into())
        .with_max_fee_per_blob_gas(provider.get_gas_price().await.unwrap() + 1)
        .max_fee_per_gas(eip1559.max_fee_per_gas)
        .max_priority_fee_per_gas(eip1559.max_priority_fee_per_gas);

    let err = provider.send_transaction(WithOtherFields::new(tx)).await.unwrap_err();
    let err = err.to_string();
    assert!(err.contains("Arc does not support blob transactions"), "{err}");
}

/// A user-supplied precompile has to be present in the blocks Anvil produces, not only in the paths
/// that answer queries. Arc mines through its own block executor, which is reached separately from
/// every other network's, so this is the one place the wiring can be missed.
///
/// Gas is the observable: calling an address that holds no precompile succeeds and does nothing, so
/// only the cost distinguishes a registered precompile from an absent one.
#[tokio::test(flavor = "multi_thread")]
async fn arc_mined_blocks_carry_injected_precompiles() {
    const PRECOMPILE: Address = address!("0x0000000000000000000000000000000000000071");
    const PRECOMPILE_GAS: u64 = 50_000;

    #[derive(Debug)]
    struct GasBurningPrecompile;

    impl PrecompileFactory for GasBurningPrecompile {
        fn precompiles(&self) -> Vec<(Address, alloy_evm::precompiles::DynPrecompile)> {
            vec![(
                PRECOMPILE,
                alloy_evm::precompiles::DynPrecompile::from(
                    |input: alloy_evm::precompiles::PrecompileInput<'_>| {
                        Ok(revm::precompile::PrecompileOutput {
                            bytes: Bytes::new(),
                            gas_used: PRECOMPILE_GAS,
                            gas_refunded: 0,
                            status: revm::precompile::PrecompileStatus::Success,
                            state_gas_used: 0,
                            reservoir: input.reservoir,
                        })
                    },
                ),
            )]
        }
    }

    let (_api, handle) =
        spawn(arc_node_config().with_precompile_factory(GasBurningPrecompile)).await;
    let provider = handle.http_provider();
    let from = handle.dev_wallets().next().unwrap().address();

    let receipt = provider
        .send_transaction(WithOtherFields::new(
            TransactionRequest::default().from(from).to(PRECOMPILE).gas_limit(200_000),
        ))
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();

    assert!(receipt.status(), "the call itself must succeed");
    assert!(
        receipt.gas_used >= PRECOMPILE_GAS,
        "mined block charged {} gas, so the injected precompile never ran",
        receipt.gas_used
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_pq_precompile_is_registered() {
    let (_api, handle) = spawn(arc_node_config()).await;
    let provider = handle.http_provider();

    let err = provider
        .call(TransactionRequest::default().to(PQ_PRECOMPILE).input(Bytes::new().into()).into())
        .await
        .unwrap_err();
    let err = err.to_string();
    assert!(err.contains("execution reverted") || err.contains("Execution reverted"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_zero_beneficiary_aborts_block_production_atomically() {
    let (api, handle) = spawn(arc_node_config()).await;
    let provider = handle.http_provider();
    let before = provider.get_block_number().await.unwrap();

    api.anvil_set_coinbase(Address::ZERO).await.unwrap();
    let err = api.try_mine_one().await.unwrap_err().to_string();

    assert!(err.contains("Arc block production aborted"), "{err}");
    assert_eq!(provider.get_block_number().await.unwrap(), before);
}

/// A genesis file is taken at face value, including its coinbase, and Arc refuses to build a block
/// on a zero beneficiary rather than substituting one.
///
/// `Genesis::coinbase` is not optional, so an omitted key and an explicit `0x0` are the same value
/// — both land here. Anvil starts either way and reports the problem where every other Arc
/// consensus-rule violation is reported: at block production, atomically, with the transactions
/// left in the pool. `anvil_setCoinbase` clears it.
#[tokio::test(flavor = "multi_thread")]
async fn arc_genesis_with_zero_coinbase_aborts_block_production() {
    let mut genesis = LOCAL_DEV.inner.genesis.clone();
    // A missing genesis coinbase deserializes to Address::ZERO.
    genesis.coinbase = Address::ZERO;

    let (api, handle) = spawn(arc_node_config().with_genesis(Some(genesis))).await;
    let provider = handle.http_provider();
    // The genesis value is used as given, not replaced by the chainspec's.
    assert_eq!(api.backend.coinbase(), Address::ZERO);

    let before = provider.get_block_number().await.unwrap();
    let err = api.try_mine_one().await.unwrap_err().to_string();
    assert!(err.contains("Arc block production aborted"), "{err}");
    assert_eq!(provider.get_block_number().await.unwrap(), before);

    // Recoverable: naming a beneficiary is enough to start producing blocks.
    api.anvil_set_coinbase(LOCAL_DEV.inner.genesis.coinbase).await.unwrap();
    api.try_mine_one().await.unwrap();
    assert_eq!(provider.get_block_number().await.unwrap(), before + 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_blocklisted_beneficiary_aborts_without_pruning_pending_transactions() {
    let (api, handle) = spawn(arc_node_config()).await;
    let provider = handle.http_provider();
    let wallets = handle.dev_wallets().collect::<Vec<_>>();
    let beneficiary = wallets[2].address();
    let sender = wallets[0].address();
    let recipient = wallets[1].address();

    mark_native_coin_blocklisted(&api, beneficiary).await;
    api.anvil_set_coinbase(beneficiary).await.unwrap();
    api.anvil_set_auto_mine(false).await.unwrap();

    let pending = provider
        .send_transaction(WithOtherFields::new(
            TransactionRequest::default().from(sender).to(recipient).value(U256::from(1)),
        ))
        .await
        .unwrap();
    let before = provider.get_block_number().await.unwrap();

    let err = api.try_mine_one().await.unwrap_err().to_string();
    assert!(err.to_ascii_lowercase().contains("blocked"), "{err}");
    assert_eq!(provider.get_block_number().await.unwrap(), before);
    assert!(provider.get_transaction_by_hash(*pending.tx_hash()).await.unwrap().is_some());

    set_storage_word(
        &api,
        NATIVE_COIN_CONTROL,
        compute_is_blocklisted_storage_slot(beneficiary),
        U256::ZERO,
    )
    .await;
    api.try_mine_one().await.unwrap();

    assert_eq!(provider.get_block_number().await.unwrap(), before + 1);
    assert!(pending.get_receipt().await.unwrap().status());
}

/// Drives an Arc block abort through the miner service rather than `evm_mine`.
///
/// The other abort tests call `EthApi::try_mine_one`, which invokes `Backend::mine_block`
/// directly and never reaches `BlockProducer::poll_next` in `service.rs`. This test keeps
/// auto-mining on so the abort is delivered as `Ok((Err(_), backend))` to the block producer,
/// covering the branch that restores `idle_backend` and re-wakes the stream.
///
/// It asserts the observable consequences: the chain does not advance, the transaction is not
/// pruned, the node keeps serving requests, and mining resumes once the failure is cleared. It
/// does not attempt to assert the absence of a busy loop, which is not cheaply observable from
/// the RPC surface.
#[tokio::test(flavor = "multi_thread")]
async fn arc_aborted_block_via_miner_service_retains_transactions_and_recovers() {
    let (api, handle) = spawn(arc_node_config()).await;
    let provider = handle.http_provider();
    let wallets = handle.dev_wallets().collect::<Vec<_>>();
    let beneficiary = wallets[2].address();
    let sender = wallets[0].address();
    let recipient = wallets[1].address();

    mark_native_coin_blocklisted(&api, beneficiary).await;
    api.anvil_set_coinbase(beneficiary).await.unwrap();

    let before = provider.get_block_number().await.unwrap();
    let first = provider
        .send_transaction(WithOtherFields::new(
            TransactionRequest::default().from(sender).to(recipient).value(U256::from(1)),
        ))
        .await
        .unwrap();

    // Give the miner service time to pick the transaction up and fail the block lifecycle.
    tokio::time::sleep(Duration::from_secs(1)).await;

    assert_eq!(
        provider.get_block_number().await.unwrap(),
        before,
        "an aborted Arc block must not advance the chain"
    );
    // Still serving requests, and the abort did not prune the transaction from the pool.
    assert!(provider.get_transaction_by_hash(*first.tx_hash()).await.unwrap().is_some());
    assert_eq!(api.txpool_status().await.unwrap().pending, 1);

    // Clearing the blocklist alone does not resume mining: `ReadyTransactionMiner` latched
    // `has_pending_txs = false` after draining the pool, so it waits for a new ready-tx
    // notification. A second transaction supplies one, and both then mine together.
    set_storage_word(
        &api,
        NATIVE_COIN_CONTROL,
        compute_is_blocklisted_storage_slot(beneficiary),
        U256::ZERO,
    )
    .await;

    let second = provider
        .send_transaction(WithOtherFields::new(
            TransactionRequest::default().from(recipient).to(sender).value(U256::from(1)),
        ))
        .await
        .unwrap();

    assert!(second.get_receipt().await.unwrap().status());
    assert!(first.get_receipt().await.unwrap().status());
    assert!(provider.get_block_number().await.unwrap() > before);
}

#[tokio::test(flavor = "multi_thread")]
async fn arc_mining_rederives_gas_limit_and_ignores_stale_block_env() {
    let (api, handle) = spawn(arc_node_config()).await;
    let provider = handle.http_provider();
    let before = provider.get_block_number().await.unwrap();
    let expected = api.backend.gas_limit();

    // Poke `block_env` to a value that disagrees with ProtocolConfig, bypassing the RPC setter. Old
    // behavior: the block executor rejected the block and production stalled. Now `do_mine_block`
    // re-derives the limit from ProtocolConfig, so the stale `block_env` is ignored, the block is
    // produced at the protocol value, and the height advances.
    api.backend.set_gas_limit(expected.saturating_sub(1));
    api.try_mine_one().await.unwrap();

    assert_eq!(provider.get_block_number().await.unwrap(), before + 1);
    let block = provider.get_block(BlockId::latest()).await.unwrap().unwrap();
    assert_eq!(block.header.gas_limit, expected);
}
