/*
 *
 * Copyright 2026 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use super::*;
use crate::client::ConnectivityState;
use crate::client::load_balancing::LbPolicy;
use crate::client::load_balancing::LbPolicyBuilder;
use crate::client::load_balancing::LbPolicyOptions;
use crate::client::load_balancing::LbState;
use crate::client::load_balancing::QueuingPicker;
use crate::client::load_balancing::SubchannelState;
use crate::client::load_balancing::endpoint_filtering;
use crate::client::load_balancing::pick_first::PickFirstConfig;
use crate::client::load_balancing::subchannel::SubchannelUpdate;
use crate::client::load_balancing::test_utils;
use crate::client::load_balancing::test_utils::StubPolicyFuncs;
use crate::client::load_balancing::test_utils::TestEnv;
use crate::client::name_resolution::Endpoint;
use crate::client::name_resolution::ResolverUpdate;
use crate::core::Address;
use crate::rt::default_runtime;

/// Verifies that configuration parsing rejects configs where a priority
/// name in `priorities` has no corresponding entry in `children`.
#[test]
fn parse_config_child_not_found() {
    let js = r#"{
  "priorities": ["child-1", "child-2", "child-3"],
  "children": {
    "child-1": {"config": [{"round_robin":{}}]},
    "child-3": {"config": [{"round_robin":{}}]}
  }
}"#;
    let builder = Builder {};
    let got = LbConfigJson::new(js).and_then(|cfg| builder.parse_config(&cfg));
    assert!(got.is_err());
}

/// Verifies that configuration parsing rejects configs where a child in
/// `children` is not listed in `priorities`.
#[test]
fn parse_config_child_not_used() {
    let js = r#"{
  "priorities": ["child-1", "child-2"],
  "children": {
    "child-1": {"config": [{"round_robin":{}}]},
    "child-2": {"config": [{"round_robin":{}}]},
    "child-3": {"config": [{"round_robin":{}}]}
  }
}"#;
    let builder = Builder {};
    let got = LbConfigJson::new(js).and_then(|cfg| builder.parse_config(&cfg));
    assert!(got.is_err());
}

/// Verifies successful parsing of a valid multi-priority configuration
/// containing multiple child policy types and `ignoreReresolutionRequests`.
#[test]
fn parse_config_success() {
    let js = r#"{
  "priorities": ["child-1", "child-2", "child-3"],
  "children": {
    "child-1": {"config": [{"round_robin":{}}], "ignoreReresolutionRequests": true},
    "child-2": {"config": [{"pick_first": {"shuffleAddressList": true}}]},
    "child-3": {"config": [{"round_robin":{}}]}
  }
}"#;
    let builder = Builder {};
    let got = LbConfigJson::new(js)
        .and_then(|cfg| builder.parse_config(&cfg))
        .unwrap();

    assert_eq!(got.priorities, vec!["child-1", "child-2", "child-3"]);
    let mut child_names: Vec<&String> = got.children.keys().collect();
    child_names.sort();
    assert_eq!(child_names, vec!["child-1", "child-2", "child-3"]);
}

#[test]
fn parse_child_config_empty_policy_fails() {
    let json = r#"{
        "config": [],
        "ignoreReresolutionRequests": false
    }"#;
    let res: Result<PriorityChildConfig, _> = serde_json::from_str(json);
    assert!(res.is_err());
}

/// Verifies that a child config without `ignoreReresolutionRequests`
/// defaults the flag to `false`, and that a child policy without its own
/// configuration parses into a builder with no config.
#[test]
fn parse_child_config_defaults() {
    let json = r#"{"config": [{"round_robin": {}}]}"#;
    let cfg: PriorityChildConfig = serde_json::from_str(json).unwrap();

    assert!(!cfg.ignore_reresolution_requests);
    assert_eq!(cfg.config.builder.name(), "round_robin");
}

/// Verifies that `ignoreReresolutionRequests` and the policy-specific
/// configuration of the child policy are parsed.
#[test]
fn parse_child_config_with_policy_config() {
    let json = r#"{
        "config": [{"pick_first": {"shuffleAddressList": true}}],
        "ignoreReresolutionRequests": true
    }"#;
    let cfg: PriorityChildConfig = serde_json::from_str(json).unwrap();

    assert!(cfg.ignore_reresolution_requests);
    assert_eq!(cfg.config.builder.name(), "pick_first");
    assert!(cfg.config.config.as_ref().is::<PickFirstConfig>());
}

/// Verifies that the first supported policy in the list is selected, as
/// specified in gRFC A24 (Section Service Config Changes).
#[test]
fn parse_child_config_picks_first_supported_policy() {
    let json = r#"{
        "config": [{"unsupported_policy": {}}, {"round_robin": {}}]
    }"#;
    let cfg: PriorityChildConfig = serde_json::from_str(json).unwrap();

    assert_eq!(cfg.config.builder.name(), "round_robin");
}

fn setup_test_env() -> TestEnv<PriorityPolicy> {
    TestEnv::new(|work_scheduler| {
        Builder {}.build(LbPolicyOptions {
            runtime: default_runtime(),
            work_scheduler,
        })
    })
}

/// Verifies that the policy produced a new picker with connectivity state
/// `want`, and returns the state containing it.
fn expect_picker_state(env: &mut TestEnv<PriorityPolicy>, want: ConnectivityState) -> LbState {
    let state = env.expect_picker_update();
    assert_eq!(state.connectivity_state, want);
    state
}

/// Advances virtual time by `duration` on the paused Tokio runtime.
///
/// Yields execution before advancing so that any newly spawned background timer
/// tasks (such as those spawned by `Timer::new`) have a chance to execute up to
/// their first `.await` point, polling their `sleep` future and registering on
/// Tokio's timer wheel. After advancing the clock, yields again to allow woken
/// timer tasks to execute their completion continuations (such as scheduling
/// work).
async fn advance_time(duration: Duration) {
    tokio::task::yield_now().await;
    tokio::time::advance(duration).await;
    tokio::task::yield_now().await;
}

/// Helper creating an Endpoint configured with a hierarchical path.
fn new_test_endpoint(child_name: &str, addr: &str) -> Endpoint {
    let endpoint = Endpoint {
        addresses: vec![Address {
            address: addr.to_string().into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    endpoint_filtering::set_path_in_endpoint(endpoint, vec![child_name.to_string()])
}

/// Registers a stub child LB policy named `policy_name`.
///
/// On every resolver update, the stub creates a subchannel for the first
/// address. On every subchannel update, it reports the subchannel's
/// connectivity state as its own (with a new picker), and requests
/// re-resolution if the subchannel is in TRANSIENT_FAILURE.
fn reg_stub(policy_name: &'static str) {
    let funcs = StubPolicyFuncs {
        resolver_update: Some(Arc::new(|data, update, _cfg, controller| {
            let addr = update
                .endpoints
                .as_ref()
                .ok()
                .and_then(|e| e.first())
                .and_then(|e| e.addresses.first())
                .cloned()
                .unwrap_or_default();
            controller.new_subchannel(&addr, data.lb_policy_options.work_scheduler.clone());
            Ok(())
        })),
        // Subchannel state changes are delivered as SubchannelUpdate work
        // items.
        work: Some(Arc::new(|_data, work_data, controller| {
            let update = work_data
                .expect("expected work data")
                .downcast::<SubchannelUpdate>()
                .expect("expected SubchannelUpdate");
            if update.state.connectivity_state == ConnectivityState::TransientFailure {
                controller.request_resolution();
            }
            controller.update_picker(LbState {
                connectivity_state: update.state.connectivity_state,
                picker: Arc::new(QueuingPicker {}),
            });
        })),
        ..Default::default()
    };
    test_utils::reg_stub_policy(policy_name, funcs);
}

/// Verifies that an empty priority list immediately reports
/// TRANSIENT_FAILURE with an explanatory failing picker.
#[tokio::test]
async fn empty_priorities_reports_transient_failure() {
    let mut env = setup_test_env();
    let js = r#"{
  "priorities": [],
  "children": {}
}"#;
    let cfg = Builder {}
        .parse_config(&LbConfigJson::new(js).unwrap())
        .unwrap();
    let update = ResolverUpdate {
        attributes: Default::default(),
        endpoints: Ok(vec![]),
        service_config: Ok(None),
        resolution_note: None,
    };
    env.policy
        .resolver_update(update, &cfg, &mut env.tcc)
        .unwrap();

    expect_picker_state(&mut env, ConnectivityState::TransientFailure);
    env.expect_no_events();
}

/// Verifies that when a high-priority child is READY, traffic routes to it
/// and adding or removing lower priorities does not cause connection
/// churn or initialize unused children.
#[tokio::test]
async fn high_priority_ready_and_add_remove_lower() {
    reg_stub("stub_hr_0");
    reg_stub("stub_hr_1");
    reg_stub("stub_hr_2");

    let mut env = setup_test_env();

    let js = r#"{
  "priorities": ["child-0", "child-1"],
  "children": {
    "child-0": {"config": [{"stub_hr_0": {}}]},
    "child-1": {"config": [{"stub_hr_1": {}}]}
  }
}"#;
    let cfg = Builder {}
        .parse_config(&LbConfigJson::new(js).unwrap())
        .unwrap();
    let update = ResolverUpdate {
        attributes: Default::default(),
        endpoints: Ok(vec![
            new_test_endpoint("child-0", "127.0.0.1:8000"),
            new_test_endpoint("child-1", "127.0.0.1:8001"),
        ]),
        service_config: Ok(None),
        resolution_note: None,
    };
    env.policy
        .resolver_update(update, &cfg, &mut env.tcc)
        .unwrap();

    // child-0 should be lazily instantiated and connecting. child-1 should NOT
    // be created while child-0 is connecting.
    let sc0 = env.expect_new_subchannel();
    expect_picker_state(&mut env, ConnectivityState::Connecting);
    env.expect_no_events();

    // Make child-0 Ready.
    env.send_subchannel_update(&sc0, &SubchannelState::ready());
    expect_picker_state(&mut env, ConnectivityState::Ready);
    env.expect_no_events();

    // Add child-2 to priorities.
    let js2 = r#"{
  "priorities": ["child-0", "child-1", "child-2"],
  "children": {
    "child-0": {"config": [{"stub_hr_0": {}}]},
    "child-1": {"config": [{"stub_hr_1": {}}]},
    "child-2": {"config": [{"stub_hr_2": {}}]}
  }
}"#;
    let cfg2 = Builder {}
        .parse_config(&LbConfigJson::new(js2).unwrap())
        .unwrap();
    let update2 = ResolverUpdate {
        attributes: Default::default(),
        endpoints: Ok(vec![
            new_test_endpoint("child-0", "127.0.0.1:8000"),
            new_test_endpoint("child-1", "127.0.0.1:8001"),
            new_test_endpoint("child-2", "127.0.0.1:8002"),
        ]),
        service_config: Ok(None),
        resolution_note: None,
    };
    env.policy
        .resolver_update(update2, &cfg2, &mut env.tcc)
        .unwrap();

    // The update is forwarded to child-0, whose stub creates a subchannel for
    // every resolver update.  child-0 is still Ready, so the picker is
    // unchanged. child-1 and child-2 should still not be created.
    env.expect_new_subchannel();
    env.expect_no_events();
}

/// Verifies priority failover when the primary child enters
/// TRANSIENT_FAILURE, and failback with deactivation when it recovers.
#[tokio::test]
async fn switch_priority_failover_and_failback() {
    reg_stub("stub_sp_0");
    reg_stub("stub_sp_1");

    let mut env = setup_test_env();

    let js = r#"{
  "priorities": ["child-0", "child-1"],
  "children": {
    "child-0": {"config": [{"stub_sp_0": {}}]},
    "child-1": {"config": [{"stub_sp_1": {}}]}
  }
}"#;
    let cfg = Builder {}
        .parse_config(&LbConfigJson::new(js).unwrap())
        .unwrap();
    let update = ResolverUpdate {
        attributes: Default::default(),
        endpoints: Ok(vec![
            new_test_endpoint("child-0", "127.0.0.1:8000"),
            new_test_endpoint("child-1", "127.0.0.1:8001"),
        ]),
        service_config: Ok(None),
        resolution_note: None,
    };
    env.policy
        .resolver_update(update, &cfg, &mut env.tcc)
        .unwrap();

    let sc0 = env.expect_new_subchannel();
    expect_picker_state(&mut env, ConnectivityState::Connecting);
    env.expect_no_events();
    env.send_subchannel_update(&sc0, &SubchannelState::ready());
    expect_picker_state(&mut env, ConnectivityState::Ready);
    env.expect_no_events();

    // Turn down child-0 with TransientFailure.
    env.send_subchannel_update(
        &sc0,
        &SubchannelState::transient_failure("connection refused"),
    );

    // child-0 requests re-resolution. Failover: child-1 is lazily created and
    // starts connecting.
    env.expect_request_resolution();
    let sc1 = env.expect_new_subchannel();
    expect_picker_state(&mut env, ConnectivityState::Connecting);
    env.expect_no_events();

    // Make child-1 Ready.
    env.send_subchannel_update(&sc1, &SubchannelState::ready());
    expect_picker_state(&mut env, ConnectivityState::Ready);
    env.expect_no_events();

    // Failback: child-0 recovers to Ready and is selected again.
    env.send_subchannel_update(&sc0, &SubchannelState::ready());
    expect_picker_state(&mut env, ConnectivityState::Ready);
    env.expect_no_events();

    // child-1 is deactivated with a 15-minute timer.
    assert!(matches!(
        env.policy.child("child-1").unwrap().state(),
        Some(ChildState::Deactivated(_))
    ));
}

/// Verifies that when a primary child remains in CONNECTING, the 10-second
/// failover timer expires and triggers failover to the next priority.
#[tokio::test(start_paused = true)]
async fn init_timeout_failover() {
    reg_stub("stub_to_0");
    reg_stub("stub_to_1");

    let mut env = setup_test_env();

    let js = r#"{
  "priorities": ["child-0", "child-1"],
  "children": {
    "child-0": {"config": [{"stub_to_0": {}}]},
    "child-1": {"config": [{"stub_to_1": {}}]}
  }
}"#;
    let cfg = Builder {}
        .parse_config(&LbConfigJson::new(js).unwrap())
        .unwrap();
    let update = ResolverUpdate {
        attributes: Default::default(),
        endpoints: Ok(vec![
            new_test_endpoint("child-0", "127.0.0.1:8000"),
            new_test_endpoint("child-1", "127.0.0.1:8001"),
        ]),
        service_config: Ok(None),
        resolution_note: None,
    };
    env.policy
        .resolver_update(update, &cfg, &mut env.tcc)
        .unwrap();

    // child-0 is connecting; child-1 is not initialized before the timeout.
    env.expect_new_subchannel();
    expect_picker_state(&mut env, ConnectivityState::Connecting);
    env.expect_no_events();

    // Advance time by 5 seconds (less than 10s timeout).
    advance_time(Duration::from_secs(5)).await;
    env.expect_no_events();

    assert!(
        matches!(
            env.policy.child("child-0").unwrap().state(),
            Some(ChildState::Connecting(_))
        ),
        "child-0 should still be in Connecting state after 5 seconds"
    );

    // Advance time by another 6 seconds (total 11s > 10s timeout).
    advance_time(Duration::from_secs(6)).await;

    let data = env.expect_schedule_work();
    env.policy.work(data, &mut env.tcc);

    // child-0 should now be ConnectingExpired.
    assert!(matches!(
        env.policy.child("child-0").unwrap().state(),
        Some(ChildState::ConnectingExpired)
    ));

    // child-1 should have been lazily initialized and selected.
    env.expect_new_subchannel();
    expect_picker_state(&mut env, ConnectivityState::Connecting);
    env.expect_no_events();
}

/// Verifies that receiving multiple CONNECTING updates does not reset the
/// 10-second failover timer.
#[tokio::test(start_paused = true)]
async fn connecting_to_connecting_does_not_restart_timer() {
    reg_stub("stub_c2c_0");
    reg_stub("stub_c2c_1");

    let mut env = setup_test_env();

    let js = r#"{
  "priorities": ["child-0", "child-1"],
  "children": {
    "child-0": {"config": [{"stub_c2c_0": {}}]},
    "child-1": {"config": [{"stub_c2c_1": {}}]}
  }
}"#;
    let cfg = Builder {}
        .parse_config(&LbConfigJson::new(js).unwrap())
        .unwrap();
    let update = ResolverUpdate {
        attributes: Default::default(),
        endpoints: Ok(vec![
            new_test_endpoint("child-0", "127.0.0.1:8000"),
            new_test_endpoint("child-1", "127.0.0.1:8001"),
        ]),
        service_config: Ok(None),
        resolution_note: None,
    };
    env.policy
        .resolver_update(update, &cfg, &mut env.tcc)
        .unwrap();
    let sc0 = env.expect_new_subchannel();
    expect_picker_state(&mut env, ConnectivityState::Connecting);
    env.expect_no_events();

    // Advance time by 5 seconds.
    advance_time(Duration::from_secs(5)).await;
    env.expect_no_events();

    // Send another Connecting update for child-0. Its new picker is published.
    env.send_subchannel_update(&sc0, &SubchannelState::connecting());
    expect_picker_state(&mut env, ConnectivityState::Connecting);
    env.expect_no_events();

    // Advance time by 6 seconds (total 11s from start, but only 6s from
    // 2nd update).
    advance_time(Duration::from_secs(6)).await;

    let data = env.expect_schedule_work();
    env.policy.work(data, &mut env.tcc);

    // The timer should have expired based on original start time
    // (11s > 10s).
    assert!(matches!(
        env.policy.child("child-0").unwrap().state(),
        Some(ChildState::ConnectingExpired)
    ));

    // Failover: child-1 is lazily created and selected.
    env.expect_new_subchannel();
    expect_picker_state(&mut env, ConnectivityState::Connecting);
    env.expect_no_events();
}

/// Verifies that a child transitioning from TRANSIENT_FAILURE to
/// CONNECTING enters ConnectingExpired without a new 10-second failover
/// timer.
#[tokio::test]
async fn transient_failure_to_connecting_enters_connecting_expired() {
    reg_stub("stub_tf2c_0");
    reg_stub("stub_tf2c_1");

    let mut env = setup_test_env();

    let js = r#"{
  "priorities": ["child-0", "child-1"],
  "children": {
    "child-0": {"config": [{"stub_tf2c_0": {}}]},
    "child-1": {"config": [{"stub_tf2c_1": {}}]}
  }
}"#;
    let cfg = Builder {}
        .parse_config(&LbConfigJson::new(js).unwrap())
        .unwrap();
    let update = ResolverUpdate {
        attributes: Default::default(),
        endpoints: Ok(vec![
            new_test_endpoint("child-0", "127.0.0.1:8000"),
            new_test_endpoint("child-1", "127.0.0.1:8001"),
        ]),
        service_config: Ok(None),
        resolution_note: None,
    };
    env.policy
        .resolver_update(update, &cfg, &mut env.tcc)
        .unwrap();

    let sc0 = env.expect_new_subchannel();
    expect_picker_state(&mut env, ConnectivityState::Connecting);
    env.expect_no_events();

    // child-0 goes to TransientFailure and requests re-resolution; child-1 is
    // initialized.
    env.send_subchannel_update(&sc0, &SubchannelState::transient_failure("fail"));
    env.expect_request_resolution();
    let sc1 = env.expect_new_subchannel();
    expect_picker_state(&mut env, ConnectivityState::Connecting);
    env.expect_no_events();

    // child-1 goes Ready and is chosen as the active child.
    env.send_subchannel_update(&sc1, &SubchannelState::ready());
    expect_picker_state(&mut env, ConnectivityState::Ready);
    env.expect_no_events();

    // Now child-0 attempts to connect again (TransientFailure -> Connecting).
    env.send_subchannel_update(&sc0, &SubchannelState::connecting());

    // Per gRFC A56, child-0 enters ConnectingExpired (no new 10s timer).
    assert!(matches!(
        env.policy.child("child-0").unwrap().state(),
        Some(ChildState::ConnectingExpired)
    ));

    // And child-1 (which is Ready) remains the chosen active child, so no new
    // picker is published.
    env.expect_no_events();
}

/// Verifies the 15-minute deactivation retention timer, background update
/// retention, child pruning upon timer expiration, and subsequent clean
/// reactivation without panic.
#[tokio::test(start_paused = true)]
async fn deactivation_and_reactivation() {
    reg_stub("stub_dr_0");
    reg_stub("stub_dr_1");

    let mut env = setup_test_env();

    let js = r#"{
  "priorities": ["child-0", "child-1"],
  "children": {
    "child-0": {"config": [{"stub_dr_0": {}}]},
    "child-1": {"config": [{"stub_dr_1": {}}]}
  }
}"#;
    let cfg = Builder {}
        .parse_config(&LbConfigJson::new(js).unwrap())
        .unwrap();
    let update = ResolverUpdate {
        attributes: Default::default(),
        endpoints: Ok(vec![
            new_test_endpoint("child-0", "127.0.0.1:8000"),
            new_test_endpoint("child-1", "127.0.0.1:8001"),
        ]),
        service_config: Ok(None),
        resolution_note: None,
    };
    env.policy
        .resolver_update(update, &cfg, &mut env.tcc)
        .unwrap();

    let sc0 = env.expect_new_subchannel();
    expect_picker_state(&mut env, ConnectivityState::Connecting);
    env.expect_no_events();

    // child-0 fails -> failover to child-1.
    env.send_subchannel_update(&sc0, &SubchannelState::transient_failure("fail"));
    env.expect_request_resolution();
    let sc1 = env.expect_new_subchannel();
    expect_picker_state(&mut env, ConnectivityState::Connecting);
    env.expect_no_events();
    env.send_subchannel_update(&sc1, &SubchannelState::ready());
    expect_picker_state(&mut env, ConnectivityState::Ready);
    env.expect_no_events();

    // child-0 recovers -> failback to child-0.
    env.send_subchannel_update(&sc0, &SubchannelState::ready());
    expect_picker_state(&mut env, ConnectivityState::Ready);
    env.expect_no_events();

    // child-1 is deactivated with a 15-minute timer.
    assert!(matches!(
        env.policy.child("child-1").unwrap().state(),
        Some(ChildState::Deactivated(_))
    ));

    // While deactivated, background updates to child-1 do NOT cancel
    // deactivation, and do not affect the picker.
    env.send_subchannel_update(&sc1, &SubchannelState::connecting());
    env.expect_no_events();
    assert!(matches!(
        env.policy.child("child-1").unwrap().state(),
        Some(ChildState::Deactivated(_))
    ));
    // Advance time past 15 minutes (901 seconds).
    advance_time(Duration::from_secs(15 * 60 + 1)).await;

    let data = env.expect_schedule_work();
    env.policy.work(data, &mut env.tcc);
    env.expect_no_events();

    // child-1 should now have had its child policy deleted, but remain in the
    // child list.
    assert!(env.policy.child("child-1").unwrap().state().is_none());

    // Now child-0 fails again. child-1 should be re-created.
    env.send_subchannel_update(&sc0, &SubchannelState::transient_failure("fail again"));
    env.expect_request_resolution();
    env.expect_new_subchannel();
    expect_picker_state(&mut env, ConnectivityState::Connecting);
    env.expect_no_events();
}

/// Verifies that ignoreReresolutionRequests config correctly filters
/// re-resolution requests from child policies.
#[tokio::test]
async fn ignore_reresolution_requests_configuration() {
    reg_stub("stub_irr_0");
    reg_stub("stub_irr_1");

    let mut env = setup_test_env();

    // child-0 has ignoreReresolutionRequests: true
    // child-1 has ignoreReresolutionRequests: false
    let js = r#"{
  "priorities": ["child-0", "child-1"],
  "children": {
    "child-0": {"config": [{"stub_irr_0": {}}], "ignoreReresolutionRequests": true},
    "child-1": {"config": [{"stub_irr_1": {}}], "ignoreReresolutionRequests": false}
  }
}"#;
    let cfg = Builder {}
        .parse_config(&LbConfigJson::new(js).unwrap())
        .unwrap();
    let update = ResolverUpdate {
        attributes: Default::default(),
        endpoints: Ok(vec![
            new_test_endpoint("child-0", "127.0.0.1:8000"),
            new_test_endpoint("child-1", "127.0.0.1:8001"),
        ]),
        service_config: Ok(None),
        resolution_note: None,
    };
    env.policy
        .resolver_update(update, &cfg, &mut env.tcc)
        .unwrap();

    let sc0 = env.expect_new_subchannel();
    expect_picker_state(&mut env, ConnectivityState::Connecting);
    env.expect_no_events();

    // child-0 enters TransientFailure (our stub calls request_resolution()).
    env.send_subchannel_update(&sc0, &SubchannelState::transient_failure("fail"));
    // Since child-0 has ignoreReresolutionRequests = true, tcc does NOT
    // receive RequestResolution. Failover occurs to child-1.
    let sc1 = env.expect_new_subchannel();
    expect_picker_state(&mut env, ConnectivityState::Connecting);
    env.expect_no_events();

    // child-1 enters TransientFailure (our stub calls request_resolution()).
    env.send_subchannel_update(&sc1, &SubchannelState::transient_failure("fail"));
    // Since child-1 has ignoreReresolutionRequests = false, tcc DOES
    // receive RequestResolution. All children have failed, so the last
    // child's TransientFailure picker is published.
    env.expect_request_resolution();
    expect_picker_state(&mut env, ConnectivityState::TransientFailure);
    env.expect_no_events();
}

/// Verifies that removing a child from the configuration immediately deletes
/// it from the child list, deleting its child policy.
#[tokio::test]
async fn remove_child_from_config_deletes_immediately() {
    reg_stub("stub_rc_0");
    reg_stub("stub_rc_1");

    let mut env = setup_test_env();

    let js = r#"{
  "priorities": ["child-0", "child-1"],
  "children": {
    "child-0": {"config": [{"stub_rc_0": {}}]},
    "child-1": {"config": [{"stub_rc_1": {}}]}
  }
}"#;
    let cfg = Builder {}
        .parse_config(&LbConfigJson::new(js).unwrap())
        .unwrap();
    let update = ResolverUpdate {
        attributes: Default::default(),
        endpoints: Ok(vec![
            new_test_endpoint("child-0", "127.0.0.1:8000"),
            new_test_endpoint("child-1", "127.0.0.1:8001"),
        ]),
        service_config: Ok(None),
        resolution_note: None,
    };
    env.policy
        .resolver_update(update, &cfg, &mut env.tcc)
        .unwrap();

    env.expect_new_subchannel();
    expect_picker_state(&mut env, ConnectivityState::Connecting);
    env.expect_no_events();
    assert!(env.policy.child("child-0").is_some());
    assert!(env.policy.child("child-1").is_some());

    // Remove child-1 from configuration.
    let js2 = r#"{
  "priorities": ["child-0"],
  "children": {
    "child-0": {"config": [{"stub_rc_0": {}}]}
  }
}"#;
    let cfg2 = Builder {}
        .parse_config(&LbConfigJson::new(js2).unwrap())
        .unwrap();
    let update2 = ResolverUpdate {
        attributes: Default::default(),
        endpoints: Ok(vec![new_test_endpoint("child-0", "127.0.0.1:8000")]),
        service_config: Ok(None),
        resolution_note: None,
    };
    env.policy
        .resolver_update(update2, &cfg2, &mut env.tcc)
        .unwrap();

    // The update is forwarded to child-0, whose stub creates a subchannel for
    // every resolver update.  The picker is unchanged.
    env.expect_new_subchannel();
    env.expect_no_events();

    // child-1 must be removed immediately (gRFC A115).
    assert!(env.policy.child("child-0").is_some());
    assert!(env.policy.child("child-1").is_none());
    let priorities: Vec<&str> = env
        .policy
        .children
        .iter()
        .map(|child_data| child_data.name.as_str())
        .collect();
    assert_eq!(priorities, vec!["child-0"]);
}

/// Verifies that PriorityPolicy::work drops its own PriorityTimerWork (without
/// forwarding to a child policy) and correctly forwards child balancer work
/// items to the child policy that scheduled them.
#[tokio::test]
async fn work_item_filtering_drops_timer_work_and_forwards_child_work() {
    let child_work_called = Arc::new(Mutex::new(false));
    let cwc_clone = child_work_called.clone();

    let funcs = StubPolicyFuncs {
        resolver_update: Some(Arc::new(move |data, update, _cfg, controller| {
            let addr = update
                .endpoints
                .as_ref()
                .ok()
                .and_then(|e| e.first())
                .and_then(|e| e.addresses.first())
                .cloned()
                .unwrap_or_default();
            controller.new_subchannel(&addr, data.lb_policy_options.work_scheduler.clone());
            // Schedule work from the child policy!
            data.lb_policy_options.work_scheduler.schedule_work(None);
            Ok(())
        })),
        work: Some(Arc::new(move |_data, _work_data, _controller| {
            *cwc_clone.lock().unwrap() = true;
        })),
        ..Default::default()
    };
    test_utils::reg_stub_policy("stub_filter_work", funcs);

    let mut env = setup_test_env();

    let js = r#"{
  "priorities": ["child-0"],
  "children": {
    "child-0": {"config": [{"stub_filter_work": {}}]}
  }
}"#;
    let cfg = Builder {}
        .parse_config(&LbConfigJson::new(js).unwrap())
        .unwrap();
    let update = ResolverUpdate {
        attributes: Default::default(),
        endpoints: Ok(vec![new_test_endpoint("child-0", "127.0.0.1:8000")]),
        service_config: Ok(None),
        resolution_note: None,
    };
    env.policy
        .resolver_update(update, &cfg, &mut env.tcc)
        .unwrap();

    env.expect_new_subchannel();
    expect_picker_state(&mut env, ConnectivityState::Connecting);

    // Deliver a PriorityTimerWork item: it should be consumed by PriorityPolicy
    // and NOT forwarded to the child policy.
    let timer_work: WorkData = Box::new(PriorityTimerWork);
    env.policy.work(Some(timer_work), &mut env.tcc);
    assert!(
        !*child_work_called.lock().unwrap(),
        "PriorityTimerWork should be dropped and not forwarded to child policy"
    );

    // Deliver the work item that the child scheduled in its resolver_update:
    // It should be forwarded to the child and invoke the child's work fn.
    let child_work = env.expect_schedule_work();
    env.expect_no_events();
    env.policy.work(child_work, &mut env.tcc);
    assert!(
        *child_work_called.lock().unwrap(),
        "Child policy work item should be forwarded to child policy"
    );
    env.expect_no_events();
}

/// Verifies that updates to inactive/background children do not cause redundant
/// `UpdatePicker` events to be published to the channel controller if the
/// active child's picker remains unchanged.
#[tokio::test]
async fn picker_updates_are_debounced_for_inactive_child_events() {
    reg_stub("stub_debounce_0");
    reg_stub("stub_debounce_1");

    let mut env = setup_test_env();

    let js = r#"{
  "priorities": ["child-0", "child-1"],
  "children": {
    "child-0": {"config": [{"stub_debounce_0": {}}]},
    "child-1": {"config": [{"stub_debounce_1": {}}]}
  }
}"#;
    let cfg = Builder {}
        .parse_config(&LbConfigJson::new(js).unwrap())
        .unwrap();
    let update = ResolverUpdate {
        attributes: Default::default(),
        endpoints: Ok(vec![
            new_test_endpoint("child-0", "127.0.0.1:8000"),
            new_test_endpoint("child-1", "127.0.0.1:8001"),
        ]),
        service_config: Ok(None),
        resolution_note: None,
    };
    env.policy
        .resolver_update(update, &cfg, &mut env.tcc)
        .unwrap();

    let sc0 = env.expect_new_subchannel();
    expect_picker_state(&mut env, ConnectivityState::Connecting);
    env.expect_no_events();

    // Transition child-0 to Ready; child-0 publishes Ready.
    env.send_subchannel_update(&sc0, &SubchannelState::ready());
    expect_picker_state(&mut env, ConnectivityState::Ready);
    env.expect_no_events();

    // An event that reconciles without altering the active child's picker
    // (such as exit_idle) does not re-emit an UpdatePicker event.
    env.policy.exit_idle(&mut env.tcc);
    env.expect_no_events();
}

/// Verifies that `PriorityPolicy::exit_idle` only wakes the currently selected
/// priority child (and not failed higher-priority children or deactivated
/// lower-priority children) and reconciles any picker update it produces.
#[tokio::test]
async fn exit_idle_wakes_only_current_priority() {
    let calls: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let reg_exit_idle_stub = |policy_name: &'static str, child_name: &'static str| {
        let calls = calls.clone();
        let funcs = StubPolicyFuncs {
            resolver_update: Some(Arc::new(|data, update, _cfg, controller| {
                let addr = update
                    .endpoints
                    .as_ref()
                    .ok()
                    .and_then(|e| e.first())
                    .and_then(|e| e.addresses.first())
                    .cloned()
                    .unwrap_or_default();
                controller.new_subchannel(&addr, data.lb_policy_options.work_scheduler.clone());
                Ok(())
            })),
            work: Some(Arc::new(|_data, work_data, controller| {
                let update = work_data
                    .expect("expected work data")
                    .downcast::<SubchannelUpdate>()
                    .expect("expected SubchannelUpdate");
                controller.update_picker(LbState {
                    connectivity_state: update.state.connectivity_state,
                    picker: Arc::new(QueuingPicker {}),
                });
            })),
            exit_idle: Some(Arc::new(move |_data, controller| {
                calls.lock().unwrap().push(child_name);
                controller.update_picker(LbState {
                    connectivity_state: ConnectivityState::Connecting,
                    picker: Arc::new(QueuingPicker {}),
                });
            })),
        };
        test_utils::reg_stub_policy(policy_name, funcs);
    };
    reg_exit_idle_stub("stub_exit_idle_0", "child-0");
    reg_exit_idle_stub("stub_exit_idle_1", "child-1");

    let mut env = setup_test_env();

    // Before any config is received, exit_idle is a no-op.
    env.policy.exit_idle(&mut env.tcc);
    assert!(calls.lock().unwrap().is_empty());

    let js = r#"{
  "priorities": ["child-0", "child-1"],
  "children": {
    "child-0": {"config": [{"stub_exit_idle_0": {}}]},
    "child-1": {"config": [{"stub_exit_idle_1": {}}]}
  }
}"#;
    let cfg = Builder {}
        .parse_config(&LbConfigJson::new(js).unwrap())
        .unwrap();
    let update = ResolverUpdate {
        attributes: Default::default(),
        endpoints: Ok(vec![
            new_test_endpoint("child-0", "127.0.0.1:8000"),
            new_test_endpoint("child-1", "127.0.0.1:8001"),
        ]),
        service_config: Ok(None),
        resolution_note: None,
    };
    env.policy
        .resolver_update(update, &cfg, &mut env.tcc)
        .unwrap();

    let sc0 = env.expect_new_subchannel();
    expect_picker_state(&mut env, ConnectivityState::Connecting);
    env.expect_no_events();

    // Transition child-0 to Idle and call exit_idle: child-0 is woken and its
    // new Connecting picker is published.
    env.send_subchannel_update(&sc0, &SubchannelState::idle());
    expect_picker_state(&mut env, ConnectivityState::Idle);
    env.policy.exit_idle(&mut env.tcc);
    assert_eq!(*calls.lock().unwrap(), vec!["child-0"]);
    calls.lock().unwrap().clear();
    expect_picker_state(&mut env, ConnectivityState::Connecting);
    env.expect_no_events();

    // Fail child-0 so priority fails over to child-1, then transition child-1
    // to Idle. Calling exit_idle wakes only child-1, not child-0.
    env.send_subchannel_update(&sc0, &SubchannelState::transient_failure("fail"));
    let sc1 = env.expect_new_subchannel();
    expect_picker_state(&mut env, ConnectivityState::Connecting);
    env.send_subchannel_update(&sc1, &SubchannelState::idle());
    expect_picker_state(&mut env, ConnectivityState::Idle);
    env.policy.exit_idle(&mut env.tcc);
    assert_eq!(*calls.lock().unwrap(), vec!["child-1"]);
    calls.lock().unwrap().clear();
    expect_picker_state(&mut env, ConnectivityState::Connecting);
    env.expect_no_events();

    // Recover child-0 to Idle so priority fails back to child-0 and deactivates
    // child-1. Calling exit_idle wakes only child-0, not deactivated child-1.
    env.send_subchannel_update(&sc0, &SubchannelState::idle());
    expect_picker_state(&mut env, ConnectivityState::Idle);
    env.policy.exit_idle(&mut env.tcc);
    assert_eq!(*calls.lock().unwrap(), vec!["child-0"]);
    expect_picker_state(&mut env, ConnectivityState::Connecting);
    env.expect_no_events();
}
