// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Circle Internet Group, Inc.

use alloy_evm::{
    Database, Evm, EvmEnv, EvmFactory, eth::EthEvmContext, precompiles::PrecompilesMap,
};
use alloy_network::Ethereum;
use alloy_primitives::{B256, U256, b256};
use arc_evm::{ArcEvm, ArcEvmFactory, handler::ArcEvmHandler};
use arc_execution_config::{
    chainspec::{
        ArcChainSpec, BaseFeeConfigProvider, BlockGasLimitProvider, LOCAL_DEV,
        bundled_chainspec_for_chain_id,
    },
    gas_fee::{arc_calc_next_block_base_fee, determine_ema_parent_gas_used},
    hardforks::ArcHardfork,
    protocol_config::{determine_bounded_base_fee, expected_gas_limit, retrieve_fee_params},
};
use arc_precompiles::system_accounting::retrieve_gas_values;
use foundry_evm_hardforks::default_local_arc_hardfork;
use foundry_evm_networks::NetworkConfigs;
use foundry_fork_db::DatabaseError;
use reth_chainspec::{EthereumHardfork, ForkCondition, Hardfork};
use revm::{
    DatabaseCommit,
    context::{
        BlockEnv, ContextTr, LocalContextTr, TxEnv,
        result::{EVMError, HaltReason, ResultAndState},
    },
    context_interface::Block,
    handler::{EthFrame, EvmTr, FrameResult, Handler, instructions::EthInstructions},
    inspector::{Inspector, InspectorHandler, NoOpInspector},
    interpreter::{
        FrameInput, SharedMemory, interpreter::EthInterpreter, interpreter_action::FrameInit,
    },
    primitives::hardfork::SpecId,
};

use crate::{
    FoundryContextExt, FoundryInspectorExt,
    backend::{DatabaseExt, JournaledState},
    evm::{FoundryEvmFactory, FoundryEvmNetwork, NestedEvm},
};

#[derive(Clone, Copy, Debug, Default)]
pub struct ArcEvmNetwork;
impl FoundryEvmNetwork for ArcEvmNetwork {
    type Network = Ethereum;
    type EvmFactory = FoundryArcEvmFactory;
}

#[derive(Clone, Debug, Default)]
pub struct FoundryArcEvmFactory {
    chain_spec: Option<std::sync::Arc<ArcChainSpec>>,
}

impl FoundryArcEvmFactory {
    pub const fn new(chain_spec: std::sync::Arc<ArcChainSpec>) -> Self {
        Self { chain_spec: Some(chain_spec) }
    }

    /// Resolves a chainspec from a chain id alone, for the paths that build an EVM without an
    /// inspector to carry [`NetworkConfigs`] (fork transaction replay, precompile enumeration).
    ///
    /// A chain id Arc does not bundle means a local chain — Foundry's dev chain id, or an Anvil
    /// being forked — so it resolves to the same default the rest of Foundry uses rather than to
    /// `LOCAL_DEV`, whose schedule activates every Arc hardfork at genesis.
    fn for_chain_id(chain_id: u64) -> std::sync::Arc<ArcChainSpec> {
        bundled_chainspec_for_chain_id(chain_id)
            .unwrap_or_else(|| capped_localdev_chainspec(default_local_arc_hardfork()))
    }

    /// Resolves the chainspec for an env that carries no explicit one.
    ///
    /// The incoming `cfg_env.spec` is deliberately ignored: Arc drives the execution spec from
    /// the chainspec rather than the other way round (`prepare_env` overwrites it), so honouring a
    /// downgraded `evm_version` here would move the chain onto an Arc schedule that
    /// `parse_local_arc_hardfork` refuses to select for local execution.
    fn chain_spec_for_env(
        &self,
        evm_env: &EvmEnv<SpecId, BlockEnv>,
    ) -> std::sync::Arc<ArcChainSpec> {
        if let Some(chain_spec) = &self.chain_spec {
            return chain_spec.clone();
        }

        Self::for_chain_id(evm_env.cfg_env.chain_id)
    }

    /// Pairs the env with the Arc chainspec that will execute it, and replaces `cfg_env.spec` with
    /// the Ethereum spec that chainspec is on.
    ///
    /// The incoming spec cannot be trusted. Forge and Cast derive it from `evm_version`, a solc
    /// setting that says which instruction set to compile for, and that does not tell us which
    /// Ethereum spec the chain runs. An Arc chain always has its own schedule — the bundled
    /// chainspec applies even without a fork — so the chainspec decides here, not the compiler
    /// flag.
    fn prepare_env(
        &self,
        mut evm_env: EvmEnv<SpecId, BlockEnv>,
    ) -> (ArcEvmFactory, EvmEnv<SpecId, BlockEnv>) {
        let chain_spec = self.chain_spec_for_env(&evm_env);
        evm_env.cfg_env.spec = arc_base_spec_id_for(&chain_spec, &evm_env.block_env);
        (ArcEvmFactory::new(chain_spec), evm_env)
    }
}

/// Resolves the Arc chain spec for the selected chain or local hardfork override.
///
/// An explicit hardfork wins, then the chain's own bundled schedule, then Foundry's default — the
/// same order [`FoundryArcEvmFactory::for_chain_id`] uses, so the resolution does not depend on
/// which entry point asked.
pub fn arc_chainspec_for(networks: &NetworkConfigs, chain_id: u64) -> std::sync::Arc<ArcChainSpec> {
    networks
        .arc_hardfork()
        .map(capped_localdev_chainspec)
        .unwrap_or_else(|| FoundryArcEvmFactory::for_chain_id(chain_id))
}

/// Returns the Ethereum revm spec paired with the Arc chain schedule at this block.
///
/// Defers to the same resolver an Arc node reaches through `ConfigureEvm::evm_env`, which walks the
/// whole Ethereum schedule newest-first rather than testing a single fork. Keeping it shared means
/// a newer Ethereum hardfork on an Arc chainspec is picked up without a change here.
pub fn arc_base_spec_id_for(chain_spec: &ArcChainSpec, block: &BlockEnv) -> SpecId {
    alloy_evm::spec_by_timestamp_and_block_number(
        chain_spec,
        block.timestamp.saturating_to::<u64>(),
        block.number.saturating_to::<u64>(),
    )
}

/// Reports a configured EVM spec that Arc cannot execute.
///
/// On Arc, `evm_version` no longer selects the executing EVM — [`prepare_env`] derives that from
/// the chainspec — so it only decides which instruction set solc targets. Compiling *below* Arc's
/// spec is legitimate: the bytecode is more conservative and still runs, which is what you want
/// when one artifact is deployed to several chains. Compiling *above* it is not, because solc
/// emits instructions the chain cannot execute and the failure surfaces on-chain rather than at
/// build time.
///
/// Returns the spec Arc will execute when `configured` is newer than it, and `None` otherwise —
/// including for every non-Arc network.
///
/// Resolution uses the chain's *maximum* Ethereum spec — every timestamp/block activation applied,
/// not the genesis spec. The bundled devnet and testnet chainspecs activate Osaka at a future
/// timestamp (only localdev/mainnet activate it at 0), so a genesis-time resolution would report
/// Prague there and wrongly reject Osaka bytecode that those chains execute once past the
/// activation. The guard should only reject a spec above what the chain can *ever* execute, so it
/// evaluates activation at the far future.
///
/// [`prepare_env`]: FoundryArcEvmFactory::prepare_env
pub fn arc_unsupported_evm_spec(
    networks: &NetworkConfigs,
    chain_id: u64,
    configured: SpecId,
) -> Option<SpecId> {
    if !networks.is_arc() {
        return None;
    }
    let max_activation = BlockEnv {
        number: U256::from(u64::MAX),
        timestamp: U256::from(u64::MAX),
        ..Default::default()
    };
    let arc_spec = arc_base_spec_id_for(&arc_chainspec_for(networks, chain_id), &max_activation);
    (configured > arc_spec).then_some(arc_spec)
}

/// Returns the localdev chainspec with every Arc hardfork after `hardfork` deactivated.
///
/// The cap works by writing [`ForkCondition::Never`] over the later forks, so it has to name every
/// fork that exists: `LOCAL_DEV` activates all of them at genesis, and one left unwritten stays
/// active past the cap. Walking [`ArcHardfork::VARIANTS`] keeps that true for forks added upstream
/// without a matching change here — a hand-written list would silently let the newest one through.
///
/// This does assume the enum stays ordered oldest-to-newest, which is how Arc declares it and how
/// `ForkCondition` comparisons elsewhere read it.
pub fn capped_localdev_chainspec(hardfork: ArcHardfork) -> std::sync::Arc<ArcChainSpec> {
    let mut inner = LOCAL_DEV.inner.clone();
    for fork in ArcHardfork::VARIANTS {
        let condition = if *fork <= hardfork {
            // Arc activates Zero7 and later by timestamp to keep EIP-2124 fork ids stable across
            // mixed-version peers; the forks before it are block-activated on this schedule.
            if *fork >= ArcHardfork::Zero7 {
                ForkCondition::Timestamp(0)
            } else {
                ForkCondition::Block(0)
            }
        } else {
            ForkCondition::Never
        };
        inner.hardforks.insert(fork.boxed(), condition);
    }
    inner.hardforks.insert(
        EthereumHardfork::Osaka.boxed(),
        if hardfork >= ArcHardfork::Zero5 {
            ForkCondition::Timestamp(0)
        } else {
            ForkCondition::Never
        },
    );
    std::sync::Arc::new(ArcChainSpec::new(inner))
}

/// Returns the base fee an Arc chain starts at, taken from the chainspec genesis.
///
/// Arc clamps every computed base fee to the `ProtocolConfig` `minBaseFee`/`maxBaseFee` range, so
/// seeding a chain below that floor makes the first mined block jump straight to it — the header of
/// the pre-jump block then advertises a base fee that the next block will reject. Reading the floor
/// from the genesis `ProtocolConfig` keeps the starting point protocol-consistent, and reuses the
/// node's own accessor instead of duplicating the storage layout here.
///
/// Returns `None` when the chainspec genesis carries no readable `ProtocolConfig`.
pub fn arc_initial_base_fee(chain_spec: &std::sync::Arc<ArcChainSpec>) -> Option<u64> {
    let mut db = revm::database::InMemoryDB::default();
    for (address, account) in &chain_spec.inner.genesis.alloc {
        let code = account.code.clone().map(revm::bytecode::Bytecode::new_raw);
        db.insert_account_info(
            *address,
            revm::state::AccountInfo {
                balance: account.balance,
                nonce: account.nonce.unwrap_or_default(),
                code_hash: code
                    .as_ref()
                    .map_or(revm::primitives::KECCAK_EMPTY, |code| code.hash_slow()),
                code,
                account_id: None,
            },
        );
        for (slot, value) in account.storage.clone().unwrap_or_default() {
            db.insert_account_storage(*address, slot.into(), value.into()).ok()?;
        }
    }

    let block_env = BlockEnv::default();
    let evm_env = EvmEnv::new(
        revm::context::CfgEnv::new()
            .with_chain_id(chain_spec.inner.chain.id())
            .with_spec_and_mainnet_gas_params(arc_base_spec_id_for(chain_spec, &block_env)),
        block_env,
    );
    let mut evm = ArcEvmFactory::new(chain_spec.clone()).create_evm(db, evm_env);
    let fee_params = retrieve_fee_params(&mut evm).ok()?;
    let min_base_fee = u64::try_from(fee_params.minBaseFee).ok()?;
    Some(chain_spec.base_fee_config(0).clamp_absolute(min_base_fee))
}

/// Identifies the fallback used when an Arc block header lacks its ADR-0004 fee carrier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArcNextBaseFeeSource {
    SystemAccounting,
    RecomputedFromProtocolConfig,
    ChainSpecDefaults,
}

/// Resolves the next Arc base fee from parent state without mutating that state.
///
/// Only reached when the parent header carries no ADR-0004 fee carrier, which means the parent came
/// from somewhere other than local mining: a forked block from before Zero5, a chain that is not
/// Arc, or a loaded state file. Blocks this node mined always carry it.
///
/// Answers in order:
///
/// 1. `SystemAccounting[n].nextBaseFee`, which is what upstream's assembler copies into the
///    carrier.
/// 2. Recompute — EMA over `SystemAccounting[n - 1].gasUsedSmoothed`, then the same clamps
///    `ArcBlockExecutor::compute_gas_values` applies. A parent with no recorded values reads as
///    zeros and smooths from 0, matching what upstream would compute.
/// 3. The same recompute with chainspec defaults, when `ProtocolConfig` is unreadable.
///
/// Deliberately not equivalent to upstream in one place: `retrieve_gas_values` *erroring* — a
/// failed system call, not absent data — substitutes `parent_gas_used` for the smoothed value and
/// carries on, where `ArcBlockExecutor` aborts the block. A node cannot price a block on a guess
/// without splitting consensus; Anvil has no peers to disagree with, and refusing to serve a fork
/// because one state read failed is worse than a slightly-off fee.
///
/// It is also not `ArcChainSpec::next_block_base_fee`, which ignores state entirely and uses
/// chainspec defaults with `parent.gas_used`. That is upstream's header-only path, documented there
/// as unreachable once Zero5 is active; reading state gets closer to the executor.
///
/// `evm_env.block_env.gas_limit` feeds the calculation, and callers set it from their own
/// `block_gas_limit` argument rather than from `parent`. Those agree today because Arc rejects any
/// `--gas-limit` other than the protocol value and rejects `--disable-block-gas-limit` outright, so
/// a fork's limit cannot drift from its parent header. Relaxing either check would let this diverge
/// from what the executor computes.
///
/// The `DatabaseCommit` bound is inherited, not used: `retrieve_gas_values` and
/// `retrieve_fee_params` declare it on their read paths, but neither commits, and
/// `ArcEvm::transact_system_call` ends in `journal.finalize()`, which extracts the state instead
/// of writing it. Callers may therefore pass the live database.
pub fn arc_next_base_fee_from_parent_state<DB>(
    db: DB,
    chain_spec: &std::sync::Arc<ArcChainSpec>,
    evm_env: &EvmEnv<SpecId, BlockEnv>,
    parent_gas_used: u64,
) -> (u64, ArcNextBaseFeeSource)
where
    DB: Database + DatabaseCommit,
{
    let mut env = evm_env.clone();
    env.cfg_env.spec = arc_base_spec_id_for(chain_spec, &env.block_env);
    let mut evm = ArcEvmFactory::new(chain_spec.clone()).create_evm(db, env);
    let block_number = evm.block().number().saturating_to::<u64>();

    if let Ok(values) = retrieve_gas_values(block_number, &mut evm)
        && values.nextBaseFee != 0
    {
        return (values.nextBaseFee, ArcNextBaseFeeSource::SystemAccounting);
    }

    let parent_smoothed = retrieve_gas_values(block_number.saturating_sub(1), &mut evm)
        .map(|values| values.gasUsedSmoothed)
        .unwrap_or(parent_gas_used);
    let fee_params = retrieve_fee_params(&mut evm).ok();
    let base_fee_config = chain_spec.base_fee_config(block_number.saturating_add(1));
    let calc = base_fee_config.resolve_calc_params(fee_params.as_ref());
    let smoothed = determine_ema_parent_gas_used(parent_smoothed, parent_gas_used, calc.alpha)
        .unwrap_or(parent_gas_used);
    let raw = arc_calc_next_block_base_fee(
        smoothed,
        evm.block().gas_limit(),
        evm.block().basefee(),
        calc.k_rate,
        calc.inverse_elasticity_multiplier,
    );
    let bounded = fee_params.as_ref().map_or(raw, |params| determine_bounded_base_fee(params, raw));
    let next_base_fee = base_fee_config.clamp_absolute(bounded);
    let source = if fee_params.is_some() {
        ArcNextBaseFeeSource::RecomputedFromProtocolConfig
    } else {
        ArcNextBaseFeeSource::ChainSpecDefaults
    };
    (next_base_fee, source)
}

/// ERC-7201 storage slot of `ProtocolConfig.blockGasLimit`, held on the proxy at
/// [`arc_execution_config::protocol_config::PROTOCOL_CONFIG_ADDRESS`].
///
/// Mirrors the Arc node's `PROTOCOL_CONFIG_BLOCK_GAS_LIMIT_SLOT`, which is `#[cfg(test-utils)]`
/// gated upstream and therefore not importable from a normal build. The value is identical on every
/// Arc network (localdev/devnet/testnet/mainnet): an ERC-7201 slot is derived only from the fixed
/// namespace string `arc.storage.ProtocolConfig`, never from the deployment. The
/// `arc_evm_set_block_gas_limit_updates_protocol_config_and_keeps_producing` integration test
/// guards this constant against upstream drift: a wrong slot leaves ProtocolConfig at the default,
/// so the mined block would not carry the requested limit and the assertion fails.
pub const ARC_PROTOCOL_CONFIG_BLOCK_GAS_LIMIT_SLOT: B256 =
    b256!("668f09ce856848ead6cb1ddee963f15ef833cea8958030868f867aec84385203");

/// Number and timestamp of the Arc block being built on top of a parent.
#[derive(Clone, Copy, Debug)]
pub struct ArcNextBlockAttributes {
    pub number: u64,
    pub timestamp: u64,
}

/// Builds the [`EvmEnv`] for the next Arc block on top of `parent_env`, with the block gas limit
/// derived from ProtocolConfig state.
///
/// Advances `number`/`timestamp` to the next block, resolves the base Ethereum spec for *that*
/// block, then runs the ProtocolConfig system call under the resolved spec and sets
/// `block_env.gas_limit` to `expected_gas_limit(ProtocolConfig, chainspec bounds)`. Returning the
/// whole env — rather than a bare `u64` — guarantees the derive and the subsequent execution use
/// the identical number/timestamp/spec, mirroring the Arc node's proposer, which builds the
/// next-block env *before* `retrieve_fee_params`. The helper owns those
/// fields precisely so a caller cannot forget to advance the timestamp and diverge on a hardfork
/// boundary.
///
/// `basefee`/`prevrandao`/`beneficiary` are carried over from `parent_env` for the caller to
/// finalize; none affect the ProtocolConfig read. The system call only reads state (same
/// read-only-despite-`DatabaseCommit` note as [`arc_next_base_fee_from_parent_state`]), so a live
/// database may be passed here.
pub fn arc_next_block_evm_env<DB>(
    db: DB,
    chain_spec: &std::sync::Arc<ArcChainSpec>,
    parent_env: &EvmEnv<SpecId, BlockEnv>,
    attrs: ArcNextBlockAttributes,
) -> EvmEnv<SpecId, BlockEnv>
where
    DB: Database + DatabaseCommit,
{
    let mut env = parent_env.clone();
    env.block_env.number = U256::from(attrs.number);
    env.block_env.timestamp = U256::from(attrs.timestamp);
    env.cfg_env.spec = arc_base_spec_id_for(chain_spec, &env.block_env);

    let mut evm = ArcEvmFactory::new(chain_spec.clone()).create_evm(db, env.clone());
    let fee_params = retrieve_fee_params(&mut evm).ok();
    env.block_env.gas_limit =
        expected_gas_limit(fee_params.as_ref(), &chain_spec.block_gas_limit_config(attrs.number));
    env
}

impl EvmFactory for FoundryArcEvmFactory {
    type Evm<DB: Database, I: Inspector<Self::Context<DB>>> =
        <ArcEvmFactory as EvmFactory>::Evm<DB, I>;
    type Tx = TxEnv;
    type Error<DBError: core::error::Error + Send + Sync + 'static> = EVMError<DBError>;
    type HaltReason = HaltReason;
    type Context<DB: Database> = EthEvmContext<DB>;
    type Spec = SpecId;
    type BlockEnv = BlockEnv;
    type Precompiles = PrecompilesMap;

    fn create_evm<DB: Database>(
        &self,
        db: DB,
        evm_env: EvmEnv<Self::Spec, Self::BlockEnv>,
    ) -> Self::Evm<DB, NoOpInspector> {
        let (factory, evm_env) = self.prepare_env(evm_env);
        factory.create_evm(db, evm_env)
    }

    fn create_evm_with_inspector<DB: Database, I: Inspector<Self::Context<DB>>>(
        &self,
        db: DB,
        evm_env: EvmEnv<Self::Spec, Self::BlockEnv>,
        inspector: I,
    ) -> Self::Evm<DB, I> {
        let (factory, evm_env) = self.prepare_env(evm_env);
        factory.create_evm_with_inspector(db, evm_env, inspector)
    }
}

pub type ArcFoundryEvm<'db, I> = ArcEvm<
    EthEvmContext<&'db mut dyn DatabaseExt<FoundryArcEvmFactory>>,
    I,
    EthInstructions<EthInterpreter, EthEvmContext<&'db mut dyn DatabaseExt<FoundryArcEvmFactory>>>,
    PrecompilesMap,
    EthFrame,
>;

impl FoundryEvmFactory for FoundryArcEvmFactory {
    type FoundryContext<'db> = EthEvmContext<&'db mut dyn DatabaseExt<Self>>;

    type FoundryEvm<'db, I: FoundryInspectorExt<Self::FoundryContext<'db>>> = ArcFoundryEvm<'db, I>;

    fn create_foundry_evm_with_inspector<'db, I: FoundryInspectorExt<Self::FoundryContext<'db>>>(
        &self,
        db: &'db mut dyn DatabaseExt<Self>,
        evm_env: EvmEnv<Self::Spec, Self::BlockEnv>,
        inspector: I,
    ) -> Self::FoundryEvm<'db, I> {
        let factory = inspector
            .get_networks()
            .arc_hardfork()
            .map(capped_localdev_chainspec)
            .map(Self::new)
            .unwrap_or_else(|| self.clone());
        let mut evm = factory.create_evm_with_inspector(db, evm_env, inspector);
        evm.inner.ctx.cfg.tx_chain_id_check = true;
        evm.inspector().get_networks().inject_precompiles(evm.precompiles_mut());
        evm
    }

    fn tx<'evm, 'db, I: FoundryInspectorExt<Self::FoundryContext<'db>>>(
        evm: &'evm Self::FoundryEvm<'db, I>,
    ) -> &'evm Self::Tx {
        &evm.inner.ctx.tx
    }

    fn create_foundry_nested_evm<'db>(
        &self,
        db: &'db mut dyn DatabaseExt<Self>,
        evm_env: EvmEnv<Self::Spec, Self::BlockEnv>,
        inspector: &'db mut dyn FoundryInspectorExt<Self::FoundryContext<'db>>,
    ) -> Box<dyn NestedEvm<Spec = SpecId, Block = BlockEnv, Tx = TxEnv> + 'db> {
        Box::new(self.create_foundry_evm_with_inspector(db, evm_env, inspector))
    }
}

impl<'db, I: FoundryInspectorExt<EthEvmContext<&'db mut dyn DatabaseExt<FoundryArcEvmFactory>>>>
    NestedEvm for ArcFoundryEvm<'db, I>
{
    type Spec = SpecId;
    type Block = BlockEnv;
    type Tx = TxEnv;

    fn journal_inner_mut(&mut self) -> &mut JournaledState {
        &mut self.inner.ctx.journaled_state.inner
    }

    fn run_execution(&mut self, frame: FrameInput) -> Result<FrameResult, EVMError<DatabaseError>> {
        let mut handler = ArcEvmHandler::<_, EVMError<DatabaseError>>::new(self.hardfork_flags);
        let memory =
            SharedMemory::new_with_buffer(self.ctx_ref().local().shared_memory_buffer().clone());
        let first_frame_input = FrameInit { depth: 0, memory, frame_input: frame };

        let mut frame_result = handler.inspect_run_exec_loop(self, first_frame_input)?;
        handler.last_frame_result(self, &mut frame_result)?;
        Ok(frame_result)
    }

    fn transact_raw(&mut self, tx: Self::Tx) -> Result<ResultAndState, EVMError<DatabaseError>> {
        self.inner.set_tx(tx);
        let result = ArcEvmHandler::<_, EVMError<DatabaseError>>::new(self.hardfork_flags)
            .inspect_run(self)?;
        Ok(ResultAndState::new(result, self.inner.ctx.journaled_state.inner.state.clone()))
    }

    fn to_evm_env(&self) -> EvmEnv<Self::Spec, Self::Block> {
        self.ctx_ref().evm_clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arc_execution_config::{
        chain_ids::{DEVNET_CHAIN_ID, LOCALDEV_CHAIN_ID, MAINNET_CHAIN_ID, TESTNET_CHAIN_ID},
        hardforks::ArcHardforkFlags,
    };
    use foundry_evm_hardforks::LOCAL_ARC_HARDFORKS;

    /// Chain id used by `NodeConfig::test()` and `forge test`, which no Arc chainspec claims.
    const UNBUNDLED_CHAIN_ID: u64 = 31_337;

    fn env_with(chain_id: u64, spec: SpecId) -> EvmEnv<SpecId, BlockEnv> {
        EvmEnv::new(
            revm::context::CfgEnv::new()
                .with_chain_id(chain_id)
                .with_spec_and_mainnet_gas_params(spec),
            BlockEnv::default(),
        )
    }

    fn flags_of(chain_spec: &ArcChainSpec) -> ArcHardforkFlags {
        ArcHardforkFlags::from_chain_hardforks(&chain_spec.inner.hardforks, 0, 0)
    }

    /// A downgraded `evm_version` must not select an Arc hardfork that
    /// `parse_local_arc_hardfork` refuses for local execution (anything below Zero6).
    ///
    /// Arc resolves the execution spec from the chainspec, not from `evm_version`
    /// (`prepare_env` overwrites `cfg_env.spec`), so asking for Prague must not silently move the
    /// chain onto an older Arc protocol schedule.
    #[test]
    fn downgraded_evm_version_keeps_the_local_arc_schedule() {
        for chain_id in [LOCALDEV_CHAIN_ID, UNBUNDLED_CHAIN_ID] {
            let on_prague = FoundryArcEvmFactory::default()
                .chain_spec_for_env(&env_with(chain_id, SpecId::PRAGUE));
            let on_osaka = FoundryArcEvmFactory::default()
                .chain_spec_for_env(&env_with(chain_id, SpecId::OSAKA));
            assert_eq!(
                flags_of(&on_prague),
                flags_of(&on_osaka),
                "chain {chain_id}: evm_version moved the Arc schedule"
            );
            assert!(
                flags_of(&on_prague).is_active(default_local_arc_hardfork()),
                "chain {chain_id} with Prague resolved below Foundry's default Arc hardfork"
            );
        }
    }

    /// The resolver follows the chainspec past Osaka.
    ///
    /// The hand-rolled version this replaced tested Osaka alone and answered `PRAGUE` for anything
    /// else, so a newer Ethereum fork on an Arc chainspec would have executed under Osaka rules
    /// until someone extended it. Sharing the resolver an Arc node uses removes that step.
    #[test]
    fn base_spec_follows_the_chainspec_past_osaka() {
        let mut inner = LOCAL_DEV.inner.clone();
        inner.hardforks.insert(EthereumHardfork::Amsterdam.boxed(), ForkCondition::Timestamp(0));
        let beyond_osaka = std::sync::Arc::new(ArcChainSpec::new(inner));
        assert_eq!(arc_base_spec_id_for(&beyond_osaka, &BlockEnv::default()), SpecId::AMSTERDAM);

        // Unchanged for the schedules Arc actually ships.
        let shipped = capped_localdev_chainspec(default_local_arc_hardfork());
        assert_eq!(arc_base_spec_id_for(&shipped, &BlockEnv::default()), SpecId::OSAKA);
    }

    /// The localdev fallback must not shadow a chain that ships its own schedule.
    #[test]
    fn bundled_arc_chains_keep_their_own_schedule() {
        for chain_id in [MAINNET_CHAIN_ID, DEVNET_CHAIN_ID, TESTNET_CHAIN_ID] {
            let expected = bundled_chainspec_for_chain_id(chain_id).expect("bundled chainspec");
            for spec in [SpecId::PRAGUE, SpecId::OSAKA] {
                let resolved =
                    FoundryArcEvmFactory::default().chain_spec_for_env(&env_with(chain_id, spec));
                assert_eq!(
                    flags_of(&resolved),
                    flags_of(&expected),
                    "chain {chain_id} with {spec:?} did not keep its bundled schedule"
                );
            }
        }
    }

    /// Compiling below the EVM Arc executes is legitimate; compiling above it is not.
    #[test]
    fn only_an_evm_spec_newer_than_arc_executes_is_reported() {
        // Zero4 never activates Osaka, so Arc executes Prague on that schedule.
        let prague = NetworkConfigs::with_arc().with_arc_hardfork(ArcHardfork::Zero4);
        assert_eq!(
            arc_unsupported_evm_spec(&prague, UNBUNDLED_CHAIN_ID, SpecId::OSAKA),
            Some(SpecId::PRAGUE),
            "newer than Arc's spec must be reported, and must name what Arc executes"
        );
        assert_eq!(
            arc_unsupported_evm_spec(&prague, UNBUNDLED_CHAIN_ID, SpecId::PRAGUE),
            None,
            "equal to Arc's spec is accepted"
        );
        assert_eq!(
            arc_unsupported_evm_spec(&prague, UNBUNDLED_CHAIN_ID, SpecId::CANCUN),
            None,
            "older than Arc's spec is accepted"
        );

        // The local default activates Osaka, so Osaka is at Arc's spec rather than above it.
        assert_eq!(
            arc_unsupported_evm_spec(
                &NetworkConfigs::with_arc(),
                UNBUNDLED_CHAIN_ID,
                SpecId::OSAKA
            ),
            None
        );
    }

    /// devnet/testnet activate Osaka at a future timestamp, not at genesis, so the guard must
    /// resolve their maximum spec — otherwise Osaka bytecode is wrongly rejected against Prague.
    #[test]
    fn bundled_devnet_and_testnet_reach_osaka_so_osaka_is_accepted() {
        use arc_execution_config::chain_ids::{DEVNET_CHAIN_ID, TESTNET_CHAIN_ID};
        for chain_id in [DEVNET_CHAIN_ID, TESTNET_CHAIN_ID] {
            assert_eq!(
                arc_unsupported_evm_spec(&NetworkConfigs::with_arc(), chain_id, SpecId::OSAKA),
                None,
                "chain {chain_id} reaches Osaka; Osaka bytecode must be accepted"
            );
        }
    }

    #[test]
    fn non_arc_networks_are_never_reported() {
        for networks in [
            NetworkConfigs::default(),
            NetworkConfigs::with_tempo(),
            NetworkConfigs::with_optimism(),
        ] {
            for spec in [SpecId::OSAKA, SpecId::PRAGUE, SpecId::CANCUN] {
                assert_eq!(
                    arc_unsupported_evm_spec(&networks, UNBUNDLED_CHAIN_ID, spec),
                    None,
                    "{networks:?} must be unaffected by evm_version"
                );
            }
        }
    }

    /// `networks.arc_hardfork` is the channel an explicit `hardfork = "arc:zeroN"` travels on
    /// (see `EvmOpts::apply_arc_hardfork`). Selecting Zero6 must cap the schedule there rather
    /// than silently resolving to the latest local hardfork.
    #[test]
    fn networks_arc_hardfork_caps_the_resolved_schedule() {
        let networks = NetworkConfigs::with_arc().with_arc_hardfork(ArcHardfork::Zero6);
        let flags = flags_of(&arc_chainspec_for(&networks, UNBUNDLED_CHAIN_ID));

        assert!(flags.is_active(ArcHardfork::Zero6));
        assert!(!flags.is_active(ArcHardfork::Zero7), "Zero6 must not enable Zero7");
        assert!(!flags.is_active(ArcHardfork::Zero8), "Zero6 must not enable Zero8");

        // Without a selection an unbundled chain id falls back to Foundry's default fork, which is
        // the same answer the inspector-less factory path gives — not `LOCAL_DEV`'s full schedule.
        assert_eq!(
            flags_of(&arc_chainspec_for(&NetworkConfigs::default(), UNBUNDLED_CHAIN_ID)),
            flags_of(&capped_localdev_chainspec(default_local_arc_hardfork())),
        );
        assert_eq!(
            flags_of(&FoundryArcEvmFactory::for_chain_id(UNBUNDLED_CHAIN_ID)),
            flags_of(&capped_localdev_chainspec(default_local_arc_hardfork())),
        );
    }

    /// The cap has to name every fork the enum knows, or a fork added upstream keeps the
    /// `LOCAL_DEV` activation it inherits and stays live past the requested hardfork.
    #[test]
    fn capping_deactivates_every_fork_above_the_selection() {
        for selected in LOCAL_ARC_HARDFORKS {
            let flags = flags_of(&capped_localdev_chainspec(selected));
            for fork in ArcHardfork::VARIANTS.iter().copied() {
                assert_eq!(
                    flags.is_active(fork),
                    fork <= selected,
                    "selecting {selected:?}: {fork:?} activation is wrong"
                );
            }
        }
    }

    /// An explicit chainspec always wins, regardless of the incoming spec.
    #[test]
    fn explicit_chain_spec_is_not_overridden() {
        let explicit = capped_localdev_chainspec(ArcHardfork::Zero6);
        let resolved = FoundryArcEvmFactory::new(explicit.clone())
            .chain_spec_for_env(&env_with(LOCALDEV_CHAIN_ID, SpecId::PRAGUE));
        assert_eq!(flags_of(&resolved), flags_of(&explicit));
        assert!(!flags_of(&resolved).is_active(ArcHardfork::Zero7));
    }
}
