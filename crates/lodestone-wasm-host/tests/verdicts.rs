//! End-to-end contract for synchronous WASM action verdicts.
//!
//! The guest has no world handle: the host calls it from `ActionVetoes`, which is
//! the same typed, pre-commit registry the real client ask sites use. This test
//! therefore exercises the only safe shape for a guest verdict while a client
//! world guard may already be held.

mod support;

use lodestone_ecs::veto::{ActionVetoes, VerbContext};
use lodestone_model::BlockPos;
use lodestone_wasm_host::{
    Capability, CapabilitySet, PluginHost, VerdictDispatch, WasmHostPlugin,
};

// Each fixture build uses a feature-keyed target directory. Cargo serializes the
// build itself, but two tests can otherwise race between that build returning and
// opening its artifact path. Keep this focused integration file's guest builds
// serialized; the host calls remain independent assertions.
static GUEST_BUILD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn inventory_click() -> VerbContext {
    VerbContext::InventoryClick {
        window_id: 9,
        slot: 12,
        button: 1,
    }
}

fn verdict_capability() -> CapabilitySet {
    CapabilitySet::from_iter([Capability::Log, Capability::VetoActions])
}

#[test]
fn a_denial_reaches_the_real_client_veto_registry() {
    let _guest_build = GUEST_BUILD_LOCK.lock().expect("guest build lock");
    let wasm = support::build_example_plugin(&["verdict-deny"]);
    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
    host.load_file("denier", &wasm, &verdict_capability())
        .expect("the verdict guest must load");

    let mut app = lodestone_app::client_app();
    app.add_plugins(WasmHostPlugin::new(host));

    assert_eq!(
        app.world()
            .resource::<ActionVetoes>()
            .allows(&inventory_click()),
        lodestone_ecs::veto::Verdict::Deny,
        "the runtime host must register its broker with the typed veto registry the shipped client asks"
    );
}

#[test]
fn control_without_the_veto_capability_never_calls_the_guest() {
    let _guest_build = GUEST_BUILD_LOCK.lock().expect("guest build lock");
    let wasm = support::build_example_plugin(&["verdict-deny"]);
    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
    host.load_file(
        "ungranted-denier",
        &wasm,
        &CapabilitySet::from_iter([Capability::Log]),
    )
        .expect("an ungranted export is still a valid component");

    assert_eq!(
        host.verdict_all(&inventory_click()),
        VerdictDispatch::Allow,
        "a guest without veto:actions must not receive a synchronous action context"
    );
    assert_eq!(
        host.plugins()[0].failure(),
        None,
        "the control must prove the guest was skipped, not merely that it failed open"
    );
}

#[test]
fn loaded_guest_order_is_stable_and_the_first_denial_short_circuits() {
    let _guest_build = GUEST_BUILD_LOCK.lock().expect("guest build lock");
    let allow = support::build_example_plugin(&[]);
    let deny = support::build_example_plugin(&["verdict-deny"]);
    let trap = support::build_example_plugin(&["verdict-trap"]);
    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
    host.load_file("first-allow", &allow, &verdict_capability())
        .expect("allow guest loads");
    host.load_file("second-deny", &deny, &verdict_capability())
        .expect("deny guest loads");
    host.load_file("third-trap", &trap, &verdict_capability())
        .expect("trap guest loads");

    assert_eq!(host.verdict_all(&inventory_click()), VerdictDispatch::Deny);
    assert_eq!(
        host.plugins()[2].failure(),
        None,
        "the later guest must not run after the earlier loaded guest denied"
    );
}

#[test]
fn a_guest_error_denies_only_the_current_action_then_the_guest_is_unloaded() {
    let _guest_build = GUEST_BUILD_LOCK.lock().expect("guest build lock");
    let trap = support::build_example_plugin(&["verdict-trap"]);
    let deny = support::build_example_plugin(&["verdict-deny"]);
    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
    host.load_file("first-trap", &trap, &verdict_capability())
        .expect("trap guest loads");
    host.load_file("second-deny", &deny, &verdict_capability())
        .expect("deny guest loads");

    assert_eq!(
        host.verdict_all(&inventory_click()),
        VerdictDispatch::Error,
        "a failed guest fails closed for the action it was deciding"
    );
    assert!(
        host.plugins()[0].failure().is_some(),
        "the failed guest must be permanently unloaded after its failed verdict"
    );
    assert_eq!(
        host.verdict_all(&VerbContext::BlockPlace {
            pos: BlockPos::new(3, -2, 8),
        }),
        VerdictDispatch::Deny,
        "the unloaded guest must be skipped on later asks so another guest can decide them"
    );
}
