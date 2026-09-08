//! EVM hardfork definitions for Foundry.
//!
//! Provides [`FoundryHardfork`], a unified enum over Ethereum, Optimism, and Tempo hardforks
//! with `FromStr`/`Serialize`/`Deserialize` support for CLI and config usage.

use std::str::FromStr;

use alloy_chains::Chain;
use alloy_rpc_types::BlockNumberOrTag;
use foundry_compilers::artifacts::EvmVersion;
#[cfg(feature = "optimism")]
use op_revm::OpSpecId;
use revm::primitives::hardfork::SpecId;
use serde::{Deserialize, Serialize};

pub use alloy_hardforks::EthereumHardfork;
#[cfg(feature = "optimism")]
pub use alloy_op_hardforks::OpHardfork;
pub use arc_execution_config::hardforks::ArcHardfork;
pub use tempo_chainspec::hardfork::TempoHardfork;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(into = "String")]
pub enum FoundryHardfork {
    Ethereum(EthereumHardfork),
    #[cfg(feature = "optimism")]
    Optimism(OpHardfork),
    Tempo(TempoHardfork),
    Arc(ArcHardfork),
}

impl From<FoundryHardfork> for String {
    fn from(fork: FoundryHardfork) -> Self {
        match fork {
            FoundryHardfork::Ethereum(h) => format!("{h}"),
            #[cfg(feature = "optimism")]
            FoundryHardfork::Optimism(h) => format!("optimism:{h}"),
            FoundryHardfork::Tempo(h) => format!("tempo:{h}"),
            FoundryHardfork::Arc(h) => format!("arc:{h}"),
        }
    }
}

impl<'de> Deserialize<'de> for FoundryHardfork {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::from_str(&s).map_err(serde::de::Error::custom)
    }
}

impl FromStr for FoundryHardfork {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let raw = s.trim();

        let Some((ns, fork_raw)) = raw.split_once(':') else {
            return EthereumHardfork::from_str(raw)
                .map(Self::Ethereum)
                .map_err(|_| format!("unknown ethereum hardfork '{raw}'"));
        };

        let ns = ns.trim().to_ascii_lowercase();
        let fork = fork_raw.trim().to_ascii_lowercase().replace(['-', ' '], "_");

        match ns.as_str() {
            "eth" | "ethereum" => EthereumHardfork::from_str(&fork)
                .map(Self::Ethereum)
                .map_err(|_| format!("unknown ethereum hardfork '{fork_raw}'")),

            #[cfg(feature = "optimism")]
            "op" | "optimism" => OpHardfork::from_str(&fork)
                .map(Self::Optimism)
                .map_err(|_| format!("unknown optimism hardfork '{fork_raw}'")),

            "t" | "tempo" => TempoHardfork::from_str(&fork)
                .map(Self::Tempo)
                .map_err(|_| format!("unknown tempo hardfork '{fork_raw}'")),
            "arc" => parse_local_arc_hardfork(&fork).map(Self::Arc),
            _ => EthereumHardfork::from_str(&fork)
                .map(Self::Ethereum)
                .map_err(|_| format!("unknown hardfork '{raw}'")),
        }
    }
}

impl FoundryHardfork {
    pub const fn ethereum(h: EthereumHardfork) -> Self {
        Self::Ethereum(h)
    }

    #[cfg(feature = "optimism")]
    pub const fn optimism(h: OpHardfork) -> Self {
        Self::Optimism(h)
    }

    pub const fn tempo(h: TempoHardfork) -> Self {
        Self::Tempo(h)
    }

    pub const fn arc(h: ArcHardfork) -> Self {
        Self::Arc(h)
    }

    /// Returns the hardfork name without a network namespace prefix.
    pub fn name(&self) -> String {
        match self {
            Self::Ethereum(h) => format!("{h}"),
            #[cfg(feature = "optimism")]
            Self::Optimism(h) => format!("{h}"),
            Self::Tempo(h) => format!("{h}"),
            Self::Arc(h) => format!("{h}"),
        }
    }

    /// Returns the network namespace for this hardfork, or `None` for plain Ethereum.
    ///
    /// Mirrors the namespace prefix used in the `"network:hardfork"` serialization format.
    pub const fn namespace(&self) -> Option<&'static str> {
        match self {
            Self::Ethereum(_) => None,
            #[cfg(feature = "optimism")]
            Self::Optimism(_) => Some("optimism"),
            Self::Tempo(_) => Some("tempo"),
            Self::Arc(_) => Some("arc"),
        }
    }

    /// Auto-detect the active hardfork for a given chain at a specific timestamp.
    ///
    /// Tries Ethereum, then Optimism. Returns `None` for unknown chains.
    pub fn from_chain_and_timestamp(chain_id: u64, timestamp: u64) -> Option<Self> {
        let chain = Chain::from_id(chain_id);
        if let Some(fork) = EthereumHardfork::from_chain_and_timestamp(chain, timestamp) {
            return Some(Self::Ethereum(fork));
        }
        #[cfg(feature = "optimism")]
        if let Some(fork) = OpHardfork::from_chain_and_timestamp(chain, timestamp) {
            return Some(Self::Optimism(fork));
        }
        // TODO: add tempo support after https://github.com/tempoxyz/tempo/pull/3514 release
        // providing TempoHardfork::from_chain_and_timestamp
        None
    }
}

impl From<EthereumHardfork> for FoundryHardfork {
    fn from(value: EthereumHardfork) -> Self {
        Self::Ethereum(value)
    }
}

impl From<FoundryHardfork> for EthereumHardfork {
    fn from(fork: FoundryHardfork) -> Self {
        match fork {
            FoundryHardfork::Ethereum(hardfork) => hardfork,
            _ => Self::default(),
        }
    }
}

#[cfg(feature = "optimism")]
impl From<OpHardfork> for FoundryHardfork {
    fn from(value: OpHardfork) -> Self {
        Self::Optimism(value)
    }
}

#[cfg(feature = "optimism")]
impl From<FoundryHardfork> for OpHardfork {
    fn from(fork: FoundryHardfork) -> Self {
        match fork {
            FoundryHardfork::Optimism(hardfork) => hardfork,
            _ => Self::default(),
        }
    }
}

impl From<TempoHardfork> for FoundryHardfork {
    fn from(value: TempoHardfork) -> Self {
        Self::Tempo(value)
    }
}

impl From<ArcHardfork> for FoundryHardfork {
    fn from(value: ArcHardfork) -> Self {
        Self::Arc(value)
    }
}

impl From<FoundryHardfork> for TempoHardfork {
    fn from(fork: FoundryHardfork) -> Self {
        match fork {
            FoundryHardfork::Tempo(hardfork) => hardfork,
            _ => Self::default(),
        }
    }
}

impl From<FoundryHardfork> for SpecId {
    fn from(fork: FoundryHardfork) -> Self {
        match fork {
            FoundryHardfork::Ethereum(hardfork) => spec_id_from_ethereum_hardfork(hardfork),
            #[cfg(feature = "optimism")]
            FoundryHardfork::Optimism(hardfork) => spec_id_from_optimism_hardfork(hardfork).into(),
            FoundryHardfork::Tempo(hardfork) => hardfork.into(),
            FoundryHardfork::Arc(hardfork) => spec_id_from_arc_hardfork(hardfork),
        }
    }
}

/// Arc hardforks that may be selected as a local execution profile.
///
/// Earlier Arc hardforks still exist in the schedule, and a forked chain sitting at one of those
/// heights keeps using it — that path is driven by the chain's own spec, not by this list. What
/// this restricts is the explicit local override: selecting an earlier fork would run Anvil with
/// protocol rules Arc developers are not expected to develop against, and that Foundry does not
/// exercise.
pub const LOCAL_ARC_HARDFORKS: [ArcHardfork; 3] =
    [ArcHardfork::Zero6, ArcHardfork::Zero7, ArcHardfork::Zero8];

/// The Arc hardfork Foundry executes when Arc mode is on and no hardfork was selected.
///
/// This is the single answer to that question: Anvil's default, the `NetworkConfigs::with_arc()`
/// profile and the Arc EVM factory's own fallback all read it, so a node, a `forge test` run and a
/// `cast` call agree without three separate constants to keep in step.
///
/// The value is [`ArcHardfork::default()`], which the Arc node keeps pointing at the fork Arc
/// mainnet runs. That matches what every other Foundry network does — `EthereumHardfork`,
/// `OpHardfork` and `TempoHardfork` all default to their own mainnet fork — so Arc needs no special
/// case here, and the default advances on its own when mainnet does. Newer forks stay reachable
/// through `--hardfork arc:zeroN` (Anvil) or `hardfork = "arc:zeroN"` (forge/cast), which is also
/// how you get a local node on the full localdev schedule.
///
/// Not a `const`: `ArcHardfork::default()` comes from a derive, so it is not callable in a const
/// context.
pub fn default_local_arc_hardfork() -> ArcHardfork {
    ArcHardfork::default()
}

/// Parses an Arc hardfork selected for local execution, accepting an optional `arc:` prefix.
pub fn parse_local_arc_hardfork(raw: &str) -> Result<ArcHardfork, String> {
    let name = raw.strip_prefix("arc:").unwrap_or(raw);
    let hardfork =
        ArcHardfork::from_str(name).map_err(|_| format!("unknown arc hardfork '{raw}'"))?;
    if !LOCAL_ARC_HARDFORKS.contains(&hardfork) {
        // Render the accepted values the way they are typed on the command line.
        let supported = LOCAL_ARC_HARDFORKS
            .iter()
            .map(|h| format!("arc:{}", h.to_string().to_lowercase()))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "arc hardfork '{raw}' cannot be selected for local execution; supported: {supported}"
        ));
    }
    Ok(hardfork)
}

/// Maps an Arc hardfork to the Ethereum spec it extends.
///
/// Arc pairs Osaka with Zero5 and every fork since inherits it; Zero3 and Zero4 predate that
/// pairing and align to Prague. Arc has not scheduled an Ethereum fork newer than Osaka, so nothing
/// here sits above it yet — when that changes it arrives as a new Arc hardfork, which needs its own
/// arm.
///
/// This is the enum-to-spec shortcut, not the spec Arc executes. Execution takes its spec from the
/// chainspec (`foundry_evm_core::evm::arc_base_spec_id_for`), which is what a forked chain sitting
/// on an older Arc fork uses, and which overwrites `cfg_env.spec` before the EVM runs.
pub const fn spec_id_from_arc_hardfork(hardfork: ArcHardfork) -> SpecId {
    match hardfork {
        ArcHardfork::Zero3 | ArcHardfork::Zero4 => SpecId::PRAGUE,
        ArcHardfork::Zero5 | ArcHardfork::Zero6 | ArcHardfork::Zero7 | ArcHardfork::Zero8 => {
            SpecId::OSAKA
        }
        // `ArcHardfork` is `#[non_exhaustive]`, so this arm is required. It deliberately is not a
        // panic: for Arc the result only reaches `cfg_env.spec`, which the Arc EVM factory
        // overwrites from the chainspec, so aborting on a value nothing reads would be worse than
        // returning a stale one. `known_arc_hardforks_are_pinned` is what forces a real decision
        // when a fork is added upstream.
        _ => SpecId::OSAKA,
    }
}

#[cfg(feature = "optimism")]
impl From<FoundryHardfork> for OpSpecId {
    fn from(fork: FoundryHardfork) -> Self {
        match fork {
            FoundryHardfork::Optimism(hardfork) => spec_id_from_optimism_hardfork(hardfork),
            _ => Self::default(),
        }
    }
}

/// Map an `EthereumHardfork` enum into its corresponding `SpecId`.
pub fn spec_id_from_ethereum_hardfork(hardfork: EthereumHardfork) -> SpecId {
    match hardfork {
        EthereumHardfork::Frontier => SpecId::FRONTIER,
        EthereumHardfork::Homestead => SpecId::HOMESTEAD,
        EthereumHardfork::Dao => SpecId::DAO_FORK,
        EthereumHardfork::Tangerine => SpecId::TANGERINE,
        EthereumHardfork::SpuriousDragon => SpecId::SPURIOUS_DRAGON,
        EthereumHardfork::Byzantium => SpecId::BYZANTIUM,
        EthereumHardfork::Constantinople => SpecId::CONSTANTINOPLE,
        EthereumHardfork::Petersburg => SpecId::PETERSBURG,
        EthereumHardfork::Istanbul => SpecId::ISTANBUL,
        EthereumHardfork::MuirGlacier => SpecId::MUIR_GLACIER,
        EthereumHardfork::Berlin => SpecId::BERLIN,
        EthereumHardfork::London => SpecId::LONDON,
        EthereumHardfork::ArrowGlacier => SpecId::ARROW_GLACIER,
        EthereumHardfork::GrayGlacier => SpecId::GRAY_GLACIER,
        EthereumHardfork::Paris => SpecId::MERGE,
        EthereumHardfork::Shanghai => SpecId::SHANGHAI,
        EthereumHardfork::Cancun => SpecId::CANCUN,
        EthereumHardfork::Prague => SpecId::PRAGUE,
        EthereumHardfork::Osaka => SpecId::OSAKA,
        EthereumHardfork::Bpo1 | EthereumHardfork::Bpo2 => SpecId::OSAKA,
        EthereumHardfork::Bpo3 | EthereumHardfork::Bpo4 | EthereumHardfork::Bpo5 => {
            unimplemented!()
        }
        f => unreachable!("unimplemented {}", f),
    }
}

/// Map an `OptimismHardfork` enum into its corresponding `OpSpecId`.
#[cfg(feature = "optimism")]
pub fn spec_id_from_optimism_hardfork(hardfork: OpHardfork) -> OpSpecId {
    match hardfork {
        OpHardfork::Bedrock => OpSpecId::BEDROCK,
        OpHardfork::Regolith => OpSpecId::REGOLITH,
        OpHardfork::Canyon => OpSpecId::CANYON,
        OpHardfork::Ecotone => OpSpecId::ECOTONE,
        OpHardfork::Fjord => OpSpecId::FJORD,
        OpHardfork::Granite => OpSpecId::GRANITE,
        OpHardfork::Holocene => OpSpecId::HOLOCENE,
        OpHardfork::Isthmus => OpSpecId::ISTHMUS,
        OpHardfork::Interop => OpSpecId::INTEROP,
        OpHardfork::Jovian => OpSpecId::JOVIAN,
        f => unreachable!("unimplemented {}", f),
    }
}

/// Trait for converting an [`EvmVersion`] into a network-specific spec type.
pub trait FromEvmVersion: From<FoundryHardfork> {
    fn from_evm_version(version: EvmVersion) -> Self;
}

impl FromEvmVersion for SpecId {
    fn from_evm_version(version: EvmVersion) -> Self {
        match version {
            EvmVersion::Homestead => Self::HOMESTEAD,
            EvmVersion::TangerineWhistle => Self::TANGERINE,
            EvmVersion::SpuriousDragon => Self::SPURIOUS_DRAGON,
            EvmVersion::Byzantium => Self::BYZANTIUM,
            EvmVersion::Constantinople => Self::CONSTANTINOPLE,
            EvmVersion::Petersburg => Self::PETERSBURG,
            EvmVersion::Istanbul => Self::ISTANBUL,
            EvmVersion::Berlin => Self::BERLIN,
            EvmVersion::London => Self::LONDON,
            EvmVersion::Paris => Self::MERGE,
            EvmVersion::Shanghai => Self::SHANGHAI,
            EvmVersion::Cancun => Self::CANCUN,
            EvmVersion::Prague => Self::PRAGUE,
            EvmVersion::Osaka => Self::OSAKA,
        }
    }
}

#[cfg(feature = "optimism")]
impl FromEvmVersion for OpSpecId {
    fn from_evm_version(version: EvmVersion) -> Self {
        match version {
            EvmVersion::Homestead
            | EvmVersion::TangerineWhistle
            | EvmVersion::SpuriousDragon
            | EvmVersion::Byzantium
            | EvmVersion::Constantinople
            | EvmVersion::Petersburg
            | EvmVersion::Istanbul
            | EvmVersion::Berlin
            | EvmVersion::London
            | EvmVersion::Paris => Self::BEDROCK,
            EvmVersion::Shanghai => Self::CANYON,
            EvmVersion::Cancun => Self::ECOTONE,
            EvmVersion::Prague => Self::ISTHMUS,
            EvmVersion::Osaka => Self::JOVIAN,
        }
    }
}

impl FromEvmVersion for TempoHardfork {
    fn from_evm_version(_: EvmVersion) -> Self {
        Self::default()
    }
}

/// Returns the spec id derived from [`EvmVersion`] for a given spec type.
pub fn evm_spec_id<SPEC: FromEvmVersion>(evm_version: EvmVersion) -> SPEC {
    SPEC::from_evm_version(evm_version)
}

/// Convert a `BlockNumberOrTag` into an `EthereumHardfork`.
pub fn ethereum_hardfork_from_block_tag(block: impl Into<BlockNumberOrTag>) -> EthereumHardfork {
    let num = match block.into() {
        BlockNumberOrTag::Earliest => 0,
        BlockNumberOrTag::Number(num) => num,
        _ => u64::MAX,
    };

    EthereumHardfork::from_mainnet_block_number(num)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_hardforks::ethereum::mainnet::*;

    #[test]
    fn test_ethereum_spec_id_mapping() {
        assert_eq!(spec_id_from_ethereum_hardfork(EthereumHardfork::Frontier), SpecId::FRONTIER);
        assert_eq!(spec_id_from_ethereum_hardfork(EthereumHardfork::Homestead), SpecId::HOMESTEAD);

        // Test latest hardforks
        assert_eq!(spec_id_from_ethereum_hardfork(EthereumHardfork::Cancun), SpecId::CANCUN);
        assert_eq!(spec_id_from_ethereum_hardfork(EthereumHardfork::Prague), SpecId::PRAGUE);
        assert_eq!(spec_id_from_ethereum_hardfork(EthereumHardfork::Osaka), SpecId::OSAKA);
    }

    #[test]
    fn test_tempo_spec_id_mapping() {
        assert_eq!(SpecId::from(TempoHardfork::Genesis), SpecId::OSAKA);
    }

    /// Covers every variant, so the pairing is pinned rather than sampled.
    #[test]
    fn test_arc_spec_id_mapping() {
        assert_eq!(spec_id_from_arc_hardfork(ArcHardfork::Zero3), SpecId::PRAGUE);
        assert_eq!(spec_id_from_arc_hardfork(ArcHardfork::Zero4), SpecId::PRAGUE);
        assert_eq!(spec_id_from_arc_hardfork(ArcHardfork::Zero5), SpecId::OSAKA);
        assert_eq!(spec_id_from_arc_hardfork(ArcHardfork::Zero6), SpecId::OSAKA);
        assert_eq!(spec_id_from_arc_hardfork(ArcHardfork::Zero7), SpecId::OSAKA);
        assert_eq!(spec_id_from_arc_hardfork(ArcHardfork::Zero8), SpecId::OSAKA);
    }

    #[test]
    fn test_arc_hardfork_parsing() {
        assert_eq!(
            "arc:zero6".parse::<FoundryHardfork>(),
            Ok(FoundryHardfork::Arc(ArcHardfork::Zero6))
        );
        assert_eq!(
            "arc:zero8".parse::<FoundryHardfork>(),
            Ok(FoundryHardfork::Arc(ArcHardfork::Zero8))
        );
    }

    #[test]
    fn arc_hardforks_before_zero6_are_not_locally_selectable() {
        for name in ["arc:zero3", "arc:zero4", "arc:zero5"] {
            let err = name.parse::<FoundryHardfork>().unwrap_err();
            assert!(err.contains("cannot be selected for local execution"), "{err}");
            assert!(err.contains("arc:zero6, arc:zero7, arc:zero8"), "{err}");
        }
        for name in ["arc:zero6", "arc:zero7", "arc:zero8"] {
            assert!(name.parse::<FoundryHardfork>().is_ok(), "{name} should be selectable");
        }
        // Unknown names still report as unknown rather than unsupported.
        assert!("arc:zero9".parse::<FoundryHardfork>().unwrap_err().contains("unknown"));
    }

    /// The default has to be a fork `--hardfork arc:zeroN` would also accept, otherwise Foundry
    /// executes a schedule the user cannot ask for by name — and cannot opt out of either.
    ///
    /// This fails when the Arc node moves `ArcHardfork::default()` past
    /// [`LOCAL_ARC_HARDFORKS`], which is the moment the new fork needs adding to that list.
    #[test]
    fn default_local_arc_hardfork_is_locally_selectable() {
        let default = default_local_arc_hardfork();
        assert!(
            LOCAL_ARC_HARDFORKS.contains(&default),
            "Arc's default hardfork is {default:?}, which is not in LOCAL_ARC_HARDFORKS \
             ({LOCAL_ARC_HARDFORKS:?}); add it so `--hardfork arc:...` accepts it"
        );
        // Same value, reachable the way a user types it.
        assert_eq!(parse_local_arc_hardfork(&default.to_string()), Ok(default));
    }

    /// Pins the Arc hardforks this crate knows about, in order.
    ///
    /// Adding one upstream needs decisions rather than silent fallthrough:
    /// `spec_id_from_arc_hardfork` needs an arm, since its catch-all keeps returning Osaka and
    /// that stops being right the moment Arc schedules a newer Ethereum fork; and
    /// [`LOCAL_ARC_HARDFORKS`] needs the entry if the fork should be selectable locally. The
    /// ordering matters too — `capped_localdev_chainspec` caps by comparing forks, so a
    /// reordered enum would cap the wrong set.
    #[test]
    fn known_arc_hardforks_are_pinned() {
        assert_eq!(
            ArcHardfork::VARIANTS,
            [
                ArcHardfork::Zero3,
                ArcHardfork::Zero4,
                ArcHardfork::Zero5,
                ArcHardfork::Zero6,
                ArcHardfork::Zero7,
                ArcHardfork::Zero8,
            ]
            .as_slice(),
            "Arc gained or reordered a hardfork: give it an arm in `spec_id_from_arc_hardfork`, and \
             add it to `LOCAL_ARC_HARDFORKS` if it should be locally selectable"
        );
    }

    #[test]
    fn test_hardfork_from_block_tag_numbers() {
        assert_eq!(
            ethereum_hardfork_from_block_tag(MAINNET_HOMESTEAD_BLOCK - 1),
            EthereumHardfork::Frontier
        );
        assert_eq!(
            ethereum_hardfork_from_block_tag(MAINNET_LONDON_BLOCK + 1),
            EthereumHardfork::London
        );
    }

    #[test]
    fn test_from_chain_and_timestamp_ethereum_mainnet() {
        assert_eq!(
            FoundryHardfork::from_chain_and_timestamp(1, 0),
            Some(FoundryHardfork::Ethereum(EthereumHardfork::Frontier))
        );
        // Shanghai activated at timestamp 1681338455 on mainnet
        assert_eq!(
            FoundryHardfork::from_chain_and_timestamp(1, 1_681_338_455),
            Some(FoundryHardfork::Ethereum(EthereumHardfork::Shanghai))
        );
    }

    #[test]
    fn test_from_chain_and_timestamp_sepolia() {
        let sepolia_chain_id = 11155111;
        assert!(FoundryHardfork::from_chain_and_timestamp(sepolia_chain_id, u64::MAX).is_some());
    }

    #[test]
    fn test_from_chain_and_timestamp_unknown_chain() {
        assert_eq!(FoundryHardfork::from_chain_and_timestamp(999999, 0), None);
    }

    #[cfg(feature = "optimism")]
    mod optimism {
        use super::*;

        #[test]
        fn test_optimism_spec_id_mapping() {
            assert_eq!(spec_id_from_optimism_hardfork(OpHardfork::Bedrock), OpSpecId::BEDROCK);
            assert_eq!(spec_id_from_optimism_hardfork(OpHardfork::Regolith), OpSpecId::REGOLITH);

            // Test latest hardforks
            assert_eq!(spec_id_from_optimism_hardfork(OpHardfork::Holocene), OpSpecId::HOLOCENE);
            assert_eq!(spec_id_from_optimism_hardfork(OpHardfork::Interop), OpSpecId::INTEROP);
        }

        #[test]
        fn test_from_chain_and_timestamp_op_mainnet() {
            let op_chain_id = 10;
            assert!(matches!(
                FoundryHardfork::from_chain_and_timestamp(op_chain_id, u64::MAX),
                Some(FoundryHardfork::Optimism(_))
            ));
        }

        #[test]
        fn test_from_chain_and_timestamp_base() {
            let base_chain_id = 8453;
            assert!(matches!(
                FoundryHardfork::from_chain_and_timestamp(base_chain_id, u64::MAX),
                Some(FoundryHardfork::Optimism(_))
            ));
        }
    }
}
