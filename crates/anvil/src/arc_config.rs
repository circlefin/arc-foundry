// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Circle Internet Group, Inc.

use crate::{NodeConfig, eth::fees::FeeManager};
use alloy_consensus::BlockHeader;
use alloy_evm::EvmEnv;
use foundry_evm::utils::block_env_from_header;

impl NodeConfig {
    /// Returns the protocol base-fee floor for the selected Arc chain.
    ///
    /// Only meaningful for a local (non-fork) Arc node: a fork inherits its base fee from the
    /// forked chain's header instead.
    pub(crate) fn arc_initial_base_fee(&self) -> Option<u64> {
        if !self.fork_urls.is_empty() {
            return None;
        }
        let chain_spec =
            foundry_evm::core::evm::arc_chainspec_for(&self.networks, self.get_chain_id());
        foundry_evm::core::evm::arc_initial_base_fee(&chain_spec)
    }

    /// Pins the chain id and Arc-derived execution spec on an env whose block was just replaced
    /// from an external source — a forked block header, a reset, or a loaded state file.
    ///
    /// Deliberately leaves `block_env.beneficiary` alone. Whoever replaced the block already
    /// decided it: a reset carries the previous one forward, a loaded state file brings its own,
    /// and fork setup applies [`Self::apply_arc_fork_beneficiary`] separately. A zero that reaches
    /// block production is a consensus-rule violation Arc reports there, not something to be
    /// normalised away here.
    pub(crate) fn configure_arc_fork_env(&self, evm_env: &mut EvmEnv, chain_id: u64) {
        if !self.networks.is_arc() {
            return;
        }
        let chain_spec = foundry_evm::core::evm::arc_chainspec_for(&self.networks, chain_id);
        evm_env.cfg_env.chain_id = chain_id;
        evm_env.cfg_env.spec =
            foundry_evm::core::evm::arc_base_spec_id_for(&chain_spec, &evm_env.block_env);
    }

    /// Takes the beneficiary from the forked chain's own genesis.
    ///
    /// Anvil does not adopt the forked block's miner — `setup_fork_db_config` carries the previous
    /// `beneficiary` across, because Anvil mines its own blocks. For a local Arc node that previous
    /// value comes from the chainspec, but a fork only learns it is on Arc from the remote chain
    /// id, which is resolved after that point. So the chain contributes its genesis coinbase
    /// here instead, giving a forked node the same starting beneficiary a local one gets.
    ///
    /// A `--init` genesis file is applied later in `setup` and overrides this, including when it
    /// carries a zero address.
    pub(crate) fn apply_arc_fork_beneficiary(&self, evm_env: &mut EvmEnv, chain_id: u64) {
        if !self.networks.is_arc() {
            return;
        }
        let chain_spec = foundry_evm::core::evm::arc_chainspec_for(&self.networks, chain_id);
        evm_env.block_env.beneficiary = chain_spec.inner.genesis.coinbase;
    }

    pub(crate) fn next_block_base_fee_from_parent_state<DB, H>(
        &self,
        db: DB,
        evm_env: &EvmEnv,
        parent: &H,
        block_gas_limit: u64,
        fees: &FeeManager,
    ) -> u64
    where
        DB: alloy_evm::Database + revm::DatabaseCommit,
        H: BlockHeader,
    {
        if !self.networks.is_arc() {
            return fees.get_next_block_base_fee_per_gas(
                parent.gas_used(),
                block_gas_limit,
                parent.base_fee_per_gas().unwrap_or_default(),
            );
        }
        if let Some(base_fee) =
            arc_execution_config::gas_fee::decode_base_fee_from_bytes(parent.extra_data())
        {
            return base_fee;
        }

        let chain_spec =
            foundry_evm::core::evm::arc_chainspec_for(&self.networks, evm_env.cfg_env.chain_id);
        let mut parent_env = evm_env.clone();
        parent_env.block_env = block_env_from_header(parent);
        parent_env.block_env.gas_limit = block_gas_limit;
        let (next_base_fee, source) = foundry_evm::core::evm::arc_next_base_fee_from_parent_state(
            db,
            &chain_spec,
            &parent_env,
            parent.gas_used(),
        );
        tracing::warn!(
            target: "node",
            parent_block_number = parent.number(),
            ?source,
            next_base_fee,
            "Arc parent has no valid next-base-fee carrier; resolved fee from state"
        );
        next_base_fee
    }
}
