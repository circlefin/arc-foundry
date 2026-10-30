// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Circle Internet Group, Inc.

use anvil::NodeConfig;

const ARC_BEHAVIOR_TEST: &str = r#"
pragma solidity ^0.8.20;

interface Vm {
    function deal(address account, uint256 balance) external;
    function store(address target, bytes32 slot, bytes32 value) external;
}

contract ArcBehaviorTest {
    Vm constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    address constant NATIVE_COIN_CONTROL = address(uint160((uint256(0x18) << 152) | 1));
    address constant PQ_PRECOMPILE = address(uint160((uint256(0x18) << 152) | 4));

    receive() external payable {}

    function testArcBlocklistRejectsValueTransfer() public {
        vm.deal(address(this), 1 ether);
        address blocked = address(0xB0B);
        bytes32 slot = keccak256(abi.encode(blocked, uint256(2)));

        vm.store(NATIVE_COIN_CONTROL, slot, bytes32(uint256(1)));

        (bool ok,) = payable(blocked).call{value: 1}("");
        require(!ok, "blocklisted transfer succeeded");
    }

    function testArcRejectsZeroAddressValueTransfer() public {
        vm.deal(address(this), 1 ether);

        (bool ok,) = payable(address(0)).call{value: 1}("");
        require(!ok, "zero-address transfer succeeded");
    }

    function testArcPqPrecompileIsRegistered() public {
        (bool ok,) = PQ_PRECOMPILE.call{gas: 300000}("");
        require(!ok, "pq precompile missing");
    }
}
"#;

const ARC_AUTO_DETECT_SCRIPT: &str = r#"
pragma solidity ^0.8.20;

interface Vm {
    function deal(address account, uint256 balance) external;
    function store(address target, bytes32 slot, bytes32 value) external;
    function startPrank(address sender) external;
    function stopPrank() external;
}

contract ArcAutoDetectScript {
    Vm constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    address constant NATIVE_COIN_CONTROL = address(uint160((uint256(0x18) << 152) | 1));

    receive() external payable {}

    function run() external {
        address sender = address(0xA11CE);
        address blocked = address(0xB0B);
        bytes32 slot = keccak256(abi.encode(blocked, uint256(2)));

        vm.deal(sender, 1 ether);
        vm.store(NATIVE_COIN_CONTROL, slot, bytes32(uint256(1)));

        vm.startPrank(sender);
        (bool ok,) = payable(blocked).call{value: 1}("");
        vm.stopPrank();
        require(!ok, "missing Arc blocklist semantics");
    }
}
"#;

const ARC_GENESIS_ETCH_PERSISTS_TEST: &str = r#"
pragma solidity ^0.8.20;

import "forge-std/Test.sol";

// Regression test: Arc genesis predeploys are seeded once, before setUp() runs, and are never
// re-applied per transaction. A test that etches USDC to empty in setUp() must keep that change —
// if genesis were (re-)seeded after setUp(), USDC.code would become non-empty again.
contract ArcGenesisEtchPersistsTest is Test {
    address constant USDC = 0x3600000000000000000000000000000000000000;

    function setUp() public {
        vm.etch(USDC, "");
    }

    function test_etch_clears_usdc_and_is_not_overwritten() public view {
        assertEq(USDC.code.length, 0, "etch should have cleared USDC code");
    }
}
"#;

const ARC_GENESIS_PREDEPLOYS_TEST: &str = r#"
pragma solidity ^0.8.20;

interface IUSDC {
    function balanceOf(address account) external view returns (uint256);
}

contract ArcGenesisPredeploysTest {
    // NativeFiatToken (USDC) proxy — present in Arc genesis alloc from day one.
    address constant USDC = 0x3600000000000000000000000000000000000000;

    function testUSDCPredeployExistsWithoutFork() public view {
        // Without genesis-alloc seeding this reverts with
        // "call to non-contract address 0x3600…".
        // Use a fresh address (not the test contract itself) to get a zero balance.
        IUSDC(USDC).balanceOf(address(0xdead));
    }
}
"#;

// Seeding must load the *whole* genesis alloc, not just USDC: ProtocolConfig (0x3600…0001) and
// the other predeploys must have code too.
const ARC_GENESIS_MULTIPLE_PREDEPLOYS_TEST: &str = r#"
pragma solidity ^0.8.20;

contract ArcGenesisMultiplePredeploysTest {
    address constant USDC = 0x3600000000000000000000000000000000000000;
    address constant PROTOCOL_CONFIG = 0x3600000000000000000000000000000000000001;

    function test_multiple_predeploys_have_code() public view {
        require(USDC.code.length > 0, "USDC predeploy missing");
        require(PROTOCOL_CONFIG.code.length > 0, "ProtocolConfig predeploy missing");
    }
}
"#;

// Each test function runs on its own copy-on-write clone of the post-setUp baseline, so one test
// mutating a predeploy must not leak into another. If seeding were shared mutable state (or
// re-applied oddly), `test_b` could observe `test_a`'s etch. Both pass only under correct
// isolation.
const ARC_GENESIS_ISOLATION_TEST: &str = r#"
pragma solidity ^0.8.20;

import "forge-std/Test.sol";

contract ArcGenesisIsolationTest is Test {
    address constant USDC = 0x3600000000000000000000000000000000000000;

    function test_a_etches_usdc_and_sees_it_empty() public {
        vm.etch(USDC, "");
        assertEq(USDC.code.length, 0, "etch should clear USDC within this test");
    }

    function test_b_sees_usdc_intact() public view {
        assertGt(USDC.code.length, 0, "another test's etch leaked into this one");
    }
}
"#;

// Genesis is seeded before setUp(), so a storage write setUp() makes to a predeploy slot must
// survive into the test body (seeding never re-runs to reset it).
const ARC_GENESIS_STORE_PERSISTS_TEST: &str = r#"
pragma solidity ^0.8.20;

import "forge-std/Test.sol";

contract ArcGenesisStorePersistsTest is Test {
    address constant USDC = 0x3600000000000000000000000000000000000000;
    bytes32 constant SLOT = bytes32(uint256(0xC0FFEE));
    bytes32 constant VAL = bytes32(uint256(0x1234));

    function setUp() public {
        vm.store(USDC, SLOT, VAL);
    }

    function test_setup_store_on_predeploy_persists() public view {
        assertEq(vm.load(USDC, SLOT), VAL, "setUp store on predeploy was lost");
    }
}
"#;

// Side-effect guard: on a non-Arc (default) project the Arc genesis must NOT be seeded. The
// factory's `seed_genesis_predeploys` default is a no-op, so the USDC address stays empty.
const NON_ARC_NO_PREDEPLOYS_TEST: &str = r#"
pragma solidity ^0.8.20;

contract NonArcNoPredeploysTest {
    address constant USDC = 0x3600000000000000000000000000000000000000;

    function test_no_arc_predeploy_on_default_network() public view {
        require(USDC.code.length == 0, "arc predeploy leaked into a non-arc project");
    }
}
"#;

// Subcommand coverage: `forge script` in local (non-fork) mode must also see Arc predeploys, since
// it shares `Executor::new` with `forge test`. Seeding at that shared point (vs a test-only hook)
// is what covers script too.
const ARC_GENESIS_SCRIPT: &str = r#"
pragma solidity ^0.8.20;

interface IUSDC {
    function balanceOf(address account) external view returns (uint256);
}

contract ArcGenesisScript {
    address constant USDC = 0x3600000000000000000000000000000000000000;

    function run() external {
        // Without genesis seeding at the shared Executor::new, a non-fork script simulation
        // reverts here with "call to non-contract address 0x3600…".
        IUSDC(USDC).balanceOf(address(0xdead));
    }
}
"#;

// Ordering check: seeding (Executor::new) runs before setUp(), so the predeploy exists in setUp;
// a vm.etch() there must then OVERRIDE it (not just clear it) and that override must be what runs
// in the test body — i.e. seeding never re-applies on top. The stub's runtime returns uint256(42)
// for any call, so reading 42 back proves the etched code executed, not the original predeploy.
const ARC_GENESIS_ETCH_OVERRIDE_TEST: &str = r#"
pragma solidity ^0.8.20;

import "forge-std/Test.sol";

contract ArcGenesisEtchOverrideTest is Test {
    address constant USDC = 0x3600000000000000000000000000000000000000;
    // runtime bytecode: PUSH1 42; PUSH1 0; MSTORE; PUSH1 32; PUSH1 0; RETURN  -> returns uint256(42)
    bytes constant STUB = hex"602a60005260206000f3";

    function setUp() public {
        require(USDC.code.length > 0, "predeploy must exist before etch (seeding runs first)");
        vm.etch(USDC, STUB);
    }

    function test_etch_override_takes_effect() public {
        (bool ok, bytes memory ret) = USDC.call("");
        require(ok, "call to etched predeploy reverted");
        require(abi.decode(ret, (uint256)) == 42, "etched stub did not run (override lost)");
    }
}
"#;

// Same ordering check for `forge script`: forge calls the script's setUp() (needs_setup), which
// runs after Executor::new seeding, so a vm.etch() there overrides the seeded predeploy and the
// override is what run() executes.
const ARC_GENESIS_SCRIPT_ETCH: &str = r#"
pragma solidity ^0.8.20;

interface Vm {
    function etch(address target, bytes calldata newRuntimeBytecode) external;
}

contract ArcGenesisScriptEtch {
    Vm constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));
    address constant USDC = 0x3600000000000000000000000000000000000000;
    bytes constant STUB = hex"602a60005260206000f3";

    function setUp() public {
        require(USDC.code.length > 0, "predeploy must exist before etch (seeding runs first)");
        vm.etch(USDC, STUB);
    }

    function run() external {
        (bool ok, bytes memory ret) = USDC.call("");
        require(ok, "call to etched predeploy reverted");
        require(abi.decode(ret, (uint256)) == 42, "etched stub did not run (override lost)");
    }
}
"#;

const ARC_CREATE2_FACTORY_TEST: &str = r#"
pragma solidity ^0.8.20;

interface Vm {
    function computeCreate2Address(bytes32 salt, bytes32 initCodeHash, address deployer)
        external
        pure
        returns (address);
}

contract ArcCreate2Target {}

contract ArcCreate2FactoryTest {
    Vm constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));
    address constant DEFAULT_CREATE2_FACTORY = 0x4e59b44847b379578588920cA78FbF26c0B4956C;

    function testArcUsesCreate2FactoryOverride() public {
        bytes32 salt = bytes32(0);
        address expected = vm.computeCreate2Address(
            salt,
            keccak256(type(ArcCreate2Target).creationCode),
            DEFAULT_CREATE2_FACTORY
        );

        ArcCreate2Target deployed = new ArcCreate2Target{salt: salt}();
        require(address(deployed) == expected, "Arc did not use the CREATE2 factory");
    }
}
"#;

fn configure_arc_project(prj: &foundry_test_utils::TestProject) {
    prj.update_config(|config| {
        config.networks = foundry_evm_networks::NetworkConfigs::with_arc();
        config.hardfork = Some(foundry_evm::hardfork::FoundryHardfork::Arc(
            foundry_evm::hardfork::ArcHardfork::Zero6,
        ));
    });
}

forgetest_init!(forge_test_arc_genesis_etch_persists, |prj, cmd| {
    prj.add_test("ArcGenesisEtchPersists.t.sol", ARC_GENESIS_ETCH_PERSISTS_TEST);
    configure_arc_project(&prj);

    cmd.arg("test").assert_success();
});

forgetest_init!(forge_test_arc_genesis_predeploys_exist_without_fork, |prj, cmd| {
    prj.add_test("ArcGenesisPredeploys.t.sol", ARC_GENESIS_PREDEPLOYS_TEST);
    configure_arc_project(&prj);

    cmd.arg("test").assert_success();
});

forgetest_init!(forge_test_arc_genesis_seeds_multiple_predeploys, |prj, cmd| {
    prj.add_test("ArcGenesisMultiplePredeploys.t.sol", ARC_GENESIS_MULTIPLE_PREDEPLOYS_TEST);
    configure_arc_project(&prj);

    cmd.arg("test").assert_success();
});

forgetest_init!(forge_test_arc_genesis_is_isolated_across_tests, |prj, cmd| {
    prj.add_test("ArcGenesisIsolation.t.sol", ARC_GENESIS_ISOLATION_TEST);
    configure_arc_project(&prj);

    cmd.arg("test").assert_success();
});

forgetest_init!(forge_test_arc_genesis_setup_store_persists, |prj, cmd| {
    prj.add_test("ArcGenesisStorePersists.t.sol", ARC_GENESIS_STORE_PERSISTS_TEST);
    configure_arc_project(&prj);

    cmd.arg("test").assert_success();
});

forgetest_init!(forge_test_non_arc_project_has_no_arc_predeploys, |prj, cmd| {
    // Deliberately no `configure_arc_project`: default (non-Arc) network must stay unseeded.
    prj.add_test("NonArcNoPredeploys.t.sol", NON_ARC_NO_PREDEPLOYS_TEST);

    cmd.arg("test").assert_success();
});

forgetest_init!(forge_script_non_fork_seeds_arc_predeploys, |prj, cmd| {
    let script = prj.add_script("ArcGenesisScript.s.sol", ARC_GENESIS_SCRIPT);
    let script = format!("{}:ArcGenesisScript", script.display());
    configure_arc_project(&prj);

    cmd.args(["script", &script]).assert_success();
});

forgetest_init!(forge_test_arc_genesis_setup_etch_overrides_predeploy, |prj, cmd| {
    prj.add_test("ArcGenesisEtchOverride.t.sol", ARC_GENESIS_ETCH_OVERRIDE_TEST);
    configure_arc_project(&prj);

    cmd.arg("test").assert_success();
});

forgetest_init!(forge_script_arc_genesis_setup_etch_overrides_predeploy, |prj, cmd| {
    let script = prj.add_script("ArcGenesisScriptEtch.s.sol", ARC_GENESIS_SCRIPT_ETCH);
    let script = format!("{}:ArcGenesisScriptEtch", script.display());
    configure_arc_project(&prj);

    cmd.args(["script", &script]).assert_success();
});

forgetest_init!(forge_test_runs_with_arc_evm_semantics, |prj, cmd| {
    prj.add_test("ArcBehavior.t.sol", ARC_BEHAVIOR_TEST);
    configure_arc_project(&prj);

    cmd.arg("test").assert_success();
});

forgetest_init!(forge_test_arc_supports_create2_factory_override, |prj, cmd| {
    prj.add_test("ArcCreate2Factory.t.sol", ARC_CREATE2_FACTORY_TEST);
    configure_arc_project(&prj);
    prj.update_config(|config| config.always_use_create_2_factory = true);

    cmd.arg("test").assert_success();
});

forgetest_async!(forge_fork_auto_detects_arc_rpc_semantics, |prj, cmd| {
    foundry_test_utils::util::initialize(prj.root());
    prj.add_test("ArcForkBehavior.t.sol", ARC_BEHAVIOR_TEST);

    let (_api, handle) = anvil::spawn(NodeConfig::test().with_chain_id(Some(5_042_002u64))).await;
    let rpc = handle.http_endpoint();

    cmd.args(["test", "--fork-url", &rpc]).assert_success();
});

forgetest_async!(forge_script_fork_auto_detects_arc_rpc_semantics, |prj, cmd| {
    foundry_test_utils::util::initialize(prj.root());
    let script = prj.add_script("ArcAutoDetect.s.sol", ARC_AUTO_DETECT_SCRIPT);
    let script = format!("{}:ArcAutoDetectScript", script.display());

    let (_api, handle) = anvil::spawn(NodeConfig::test().with_chain_id(Some(5_042_002u64))).await;
    let rpc = handle.http_endpoint();

    cmd.args(["script", &script, "--fork-url", &rpc]).assert_success();
});
