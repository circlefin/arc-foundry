# Arc Foundry

Arc Foundry is a fork of [Foundry](https://github.com/foundry-rs/foundry) that adds
first-class support for [Arc](https://www.arc.io/). It is a **superset** of
upstream Foundry: everything upstream supports (Ethereum, Optimism,
Tempo) works unchanged, and selecting Arc switches the whole toolkit to Arc's
execution semantics.

It ships the same four tools, executed against the Arc EVM itself rather than a
separate simulation of it:

- **Forge** — compile Solidity, run unit/integration/fuzz/invariant tests, and run
  deployment and operational scripts.
- **Cast** — call contracts, send transactions, query chain data, encode/decode ABI
  values, and trace transactions.
- **Anvil** — a local node: create local chains, mine blocks, manage test accounts,
  fork remote network state, and simulate transactions.
- **Chisel** — an interactive Solidity REPL. _Not yet supported on Arc._

To keep Arc Foundry side by side with upstream Foundry, its binaries are installed
under an `arc-` prefix (`arc-forge`, `arc-cast`, `arc-anvil`). The examples below
use those names. (Chisel is not yet supported on Arc, so it is left out below.)

## Installation

Arc Foundry is distributed as precompiled binaries and as source. It is **not**
installable through `foundryup`, which only serves upstream Foundry.

If you have both Arc Foundry and upstream Foundry installed, make sure your `PATH`
is configured correctly and verify which binaries are being invoked.

### Supported platforms

| Platform | Architecture | Precompiled binaries |
| :--- | :--- | :--- |
| Linux (glibc) | x86_64 | Yes |
| Linux (glibc) | arm64 | Yes |
| macOS | Apple Silicon | Yes |
| macOS | Intel | Build from source |
| Windows | x86_64 | Build from source |
| Linux (musl / Alpine) | x86_64 / arm64 | Build from source |

Platforms without precompiled binaries are not unsupported — they are simply not
built by the release pipeline yet, and building from source works on any platform
the Rust toolchain supports.

### Precompiled binaries

Download the archive for your platform from the
[releases page](https://github.com/circlefin/arc-foundry/releases). Archives are
named `arc-foundry-<version>-<target>.tar.gz`, each published with a matching
`.sha256` file. Verify the download before extracting:

```shell
# Linux
sha256sum -c arc-foundry-<version>-x86_64-unknown-linux-gnu.tar.gz.sha256

# macOS
shasum -a 256 -c arc-foundry-<version>-aarch64-apple-darwin.tar.gz.sha256
```

Extract and put the binaries on your `PATH` under the `arc-` prefix:

```shell
tar -xzf arc-foundry-<version>-<target>.tar.gz
mkdir -p ~/.local/bin
mv forge ~/.local/bin/arc-forge
mv cast  ~/.local/bin/arc-cast
mv anvil ~/.local/bin/arc-anvil

arc-forge --version
```

### Building from source

Building from source works on any platform the Rust toolchain supports, and is
currently the only option on Intel macOS, Windows, and musl-based Linux. You need
`git` and a Rust toolchain; the repository pins its compiler version in
`rust-toolchain.toml`, so `rustup` installs the correct one automatically.

```shell
git clone https://github.com/circlefin/arc-foundry
cd arc-foundry
cargo build --release
cp target/release/{forge,cast,anvil} ~/.local/bin/arc-{forge,cast,anvil}
```

On Windows, building also requires the Visual Studio C++ build tools.

### Using Arc Foundry alongside upstream Foundry

Because the binaries are prefixed, both can be installed at the same time and
neither overwrites the other. They share the same on-disk caches and the same
`foundry.toml` format, so a project configured for Foundry needs no changes to
build with Arc Foundry.

> **CI/CD:** Do **not** use `foundry-rs/foundry-toolchain` to test Arc contracts —
> it installs upstream Foundry, which has no Arc support and will run your tests
> against Ethereum rules while reporting them as passing. Until an Arc integration
> exists, install Arc Foundry manually (download and verify a release archive, or
> build from source).

## Quick start

This assumes `arc-forge`, `arc-cast`, and `arc-anvil` are on your `PATH`.

### Start a local Arc chain

```shell
arc-anvil --network arc
```

This starts a chain running Arc rules with the protocol's system contracts already
deployed and a set of pre-funded development accounts. Blocks are produced when you
send a transaction, or on an interval you choose, rather than by consensus.

The development accounts are derived from the well-known test mnemonic
`test test test test test test test test test test test junk`. They are public and
shared by every Anvil user — safe for scripts and docs, and must never hold real
funds.

### Point a project at Arc

Arc is a profile-level setting, so a project that targets several networks keeps one
profile per network and switches between them:

```toml
[profile.default]
# Ethereum

[profile.arc]
network = "arc"
```

Select one with an environment variable:

```shell
arc-forge test                      # Ethereum
FOUNDRY_PROFILE=arc arc-forge test  # Arc
```

Selecting Arc changes execution for the whole toolkit, not only tests: `forge
script` simulations, `cast call --trace`, and `cast run` all execute under Arc
rules. A single command can also be switched with `--network arc`.

### Run against a real Arc network

Forking needs no Arc-specific configuration — Arc Foundry recognises the chain ID
and enables Arc mode with that network's own hardfork schedule:

```shell
arc-anvil --fork-url $ARC_RPC_URL
FOUNDRY_PROFILE=arc arc-forge test --fork-url $ARC_RPC_URL
```

Forking is also the only way to test behaviour across a hardfork activation, because
a local chain applies one hardfork from its first block and has no transition to
observe.

### Pin a specific hardfork

A local Arc chain runs the newest supported hardfork by default. A live network may
be running an older one, so pin the hardfork to reproduce a particular network:

```shell
arc-anvil --network arc --hardfork arc:zero6
```

The hardforks selectable for local execution are `arc:zero6`, `arc:zero7`, and
`arc:zero8`.

### Confirm the configuration took effect

No tool prints which EVM semantics are in effect, so confirm before relying on the
results. For a project, ask Forge what it resolved:

```shell
arc-forge config --json | grep -E '"network"|"arc_hardfork"'
```

It reports `"network": null` on Ethereum and `"network": "arc"` once Arc is
selected. For a running node, ask the node:

```shell
arc-cast rpc anvil_nodeInfo --rpc-url http://localhost:8545 | jq .network
```

This prints `"arc"` when Arc mode is active, and `null` otherwise.

## Differences from upstream Foundry

On Ethereum, Optimism, and Tempo, Arc Foundry behaves exactly as upstream Foundry —
an existing project needs no configuration changes to build and test with it.
Selecting Arc changes the execution semantics of **every** tool, not just Anvil.

- **Arc is auto-selected when forking an Arc network** (the chain ID is recognised).
  Local execution has no chain to recognise, so there it must be requested.
- **An exact Arc hardfork can be pinned for local execution**, reproducing the rules
  a particular network is running today rather than the newest ones. Only the three
  most recent Arc hardforks can be selected locally.
- **Blob transactions are unavailable.** Arc does not implement EIP-4844, so
  blob-carrying transactions are rejected and `cast da-estimate` reports that no
  estimate can be produced.
- **The block gas limit cannot be overridden.** Arc derives it from protocol
  configuration, so flags that change it on other networks are rejected rather than
  quietly ignored.

### Local Anvil vs a real Arc node

Contract execution matches a real Arc node: gas accounting, precompiles, and revert
conditions are identical, because Arc Foundry executes with the Arc EVM itself. What
differs is the machinery around execution:

| Behaviour | Anvil (Arc Foundry) | Arc node |
| :--- | :--- | :--- |
| Block production | On a sent transaction, or a fixed interval you choose | Decided by consensus among validators |
| Finality | Immediate; a block is never replaced | Reached through consensus voting |
| Block certificates | Not available (the RPC returns an explanatory error) | Available |
| Other participants | None; your transaction always lands next | Competes with other traffic |
| Hardfork activation | Applies from the first block; no transition | Takes effect at a block height or timestamp |
| State | In memory, lost on exit unless exported | Persisted |

Two of these are worth planning around: behaviour at a hardfork activation boundary
can only be tested by forking a real Arc network, and anything that depends on
transaction ordering behaves more forgivingly locally than on a real network.

## Configuration

Arc Foundry reads the same `foundry.toml` as upstream Foundry. Two settings decide
how Arc behaves: which network's semantics to execute under, and which Arc hardfork
applies locally.

Settings are layered, each overriding the one before: built-in defaults, then
`foundry.toml`, then `FOUNDRY_`-prefixed environment variables, then command-line
flags. Because several layers combine, ask Forge what it resolved rather than
assuming:

```shell
arc-forge config --json | grep -E '"network"|"arc_hardfork"'
```

### Selecting Arc

`network` chooses the execution semantics. Accepted values are `ethereum` (the
default), `optimism`, `tempo`, and `arc`:

```toml
[profile.arc]
network = "arc"
```

The same choice can be made for a single command with `--network arc`. (An older
form, `arc = true`, is still accepted but deprecated.)

### Pinning a hardfork

Local execution runs the newest supported Arc hardfork unless told otherwise:

```toml
[profile.arc]
hardfork = "arc:zero6"
```

Setting an Arc hardfork implies `network = "arc"`. The selectable hardforks are
`arc:zero6`, `arc:zero7`, and `arc:zero8`. A pinned hardfork also takes priority
over fork detection, so it is easy to leave one set in a profile and unknowingly run
a fork under different rules than the network's current ones.

### Settings that behave differently on Arc

- **`evm_version`** tells the Solidity compiler which instruction set to target. On
  Arc it does *not* also select the EVM tests run in — the Arc chainspec decides
  that. Setting a lower `evm_version` produces more conservative bytecode without
  changing the rules it runs under; use `hardfork` to change the rules. Keep
  `evm_version` at or below Arc's, or you emit instructions Arc cannot run.
- **Block gas limit** cannot be overridden: `--gas-limit` and
  `--disable-block-gas-limit` are rejected with an explanation.
- **Blob configuration** has no effect, as Arc does not implement EIP-4844.

## Upstream Foundry

Arc Foundry is a superset of Foundry, so all general Foundry usage, documentation,
and the Foundry Book apply. See the upstream project:

- Repository: <https://github.com/foundry-rs/foundry>
- Foundry Book: <https://getfoundry.sh>
