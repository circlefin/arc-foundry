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
