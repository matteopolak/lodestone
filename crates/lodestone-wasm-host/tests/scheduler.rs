//! End-to-end contract for the capability-gated WASM task scheduler.

mod support;

use lodestone_wasm_host::{Action, Capability, CapabilitySet, HostError, PluginHost};

fn scheduler_capabilities() -> CapabilitySet {
    CapabilitySet::from_iter([
        Capability::Log,
        Capability::ScheduleTasks,
        Capability::ActChat,
    ])
}

#[test]
fn scheduled_callbacks_are_ordered_deferred_repeated_and_cancellable() {
    let wasm = support::build_example_plugin(&["scheduler"]);
    let mut host = PluginHost::new(CapabilitySet::permissive()).expect("engine");
    host.load_file("scheduler-fixture", &wasm, &scheduler_capabilities())
        .expect("scheduler fixture must load when the import is granted");

    assert_eq!(
        host.tick_all(&[]),
        vec![
            Action::SendChat("task: repeating 1".to_owned()),
            Action::SendChat("task: zero period 1".to_owned()),
        ],
        "delay 0/1 must defer until the next host tick, and a cancelled pending task must not run"
    );
    assert_eq!(
        host.tick_all(&[]),
        vec![
            Action::SendChat("task: once".to_owned()),
            Action::SendChat("task: same deadline".to_owned()),
            Action::SendChat("task: zero period 2".to_owned()),
        ],
        "same-deadline callbacks must run in guest-local task-id order, and period zero must clamp to one"
    );
    assert_eq!(
        host.tick_all(&[]),
        vec![
            Action::SendChat("task: repeating 2".to_owned()),
            Action::SendChat("task: callback scheduled".to_owned()),
        ],
        "a repeating callback may cancel itself, while tasks it schedules are deferred to the next tick"
    );
    assert_eq!(host.tick_all(&[]), Vec::<Action>::new());
    assert_eq!(host.tick_all(&[]), Vec::<Action>::new());
}

#[test]
fn scheduler_import_is_absent_without_an_explicit_grant() {
    let wasm = support::build_example_plugin(&["scheduler"]);
    let mut default_host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
    let error = default_host
        .load_file("scheduler-fixture", &wasm, &scheduler_capabilities())
        .expect_err("default policy must withhold task scheduling");
    let HostError::CapabilityDenied { missing, .. } = error else {
        panic!("expected a policy denial, got {error:?}");
    };
    assert_eq!(missing, "schedule:tasks");
    assert!(
        default_host.is_empty(),
        "a policy-denied guest must not enter the host"
    );

    let mut host = PluginHost::new(CapabilitySet::permissive()).expect("engine");
    let requested = CapabilitySet::from_iter([Capability::Log, Capability::ActChat]);

    let error = host
        .load_file("scheduler-fixture", &wasm, &requested)
        .expect_err("using an undeclared scheduler import must fail at instantiation");
    let HostError::Instantiate { message, .. } = error else {
        panic!("expected an unresolved-import instantiation error, got {error:?}");
    };
    assert!(
        message.contains("scheduler"),
        "the refusal must name the missing scheduler interface: {message}"
    );
    assert!(host.is_empty(), "a refused guest must not enter the host");
}
