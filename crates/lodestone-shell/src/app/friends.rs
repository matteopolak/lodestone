//! Friends-service execution and the app-facing credential boundary.
//!
//! [`crate::friends_runtime`] owns scheduling and one resolved session. This
//! module owns the executor around it: native builds keep that state on a
//! dedicated worker, while browser builds keep it behind a local-task handle.
//! Both directions carry only account metadata, activity intent, and
//! [`crate::friends_runtime::FriendsView`]. A bearer token never crosses back
//! to `WindowApp`.

#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

use super::WindowApp;
use crate::friends_runtime::{
    FriendsAccount, FriendsClock, FriendsOperation, FriendsResponse, FriendsRuntime, FriendsView,
    SystemFriendsClock,
};
use lodestone_auth::friends::{
    FriendMutation, FriendsPreferences, FriendsService, FriendsServiceError, PresenceStatus,
};

/// The app-owned presentation of the Friends runtime. `WindowApp` calls
/// [`Self::sync`] once per frame; only changes in account or activity become
/// worker messages, and only the most recent credential-free view is retained.
pub(super) struct FriendsApp {
    account: Option<FriendsAccount>,
    activity: Option<PresenceStatus>,
    view: FriendsView,
    #[cfg(not(target_arch = "wasm32"))]
    worker: NativeFriendsWorker,
    #[cfg(target_arch = "wasm32")]
    worker: LocalFriendsWorker,
}

impl FriendsApp {
    pub(super) fn new() -> Self {
        Self {
            account: None,
            activity: None,
            view: FriendsView::default(),
            #[cfg(not(target_arch = "wasm32"))]
            worker: NativeFriendsWorker::new(),
            #[cfg(target_arch = "wasm32")]
            worker: LocalFriendsWorker::new(),
        }
    }

    /// Reconciles the one account the account switcher selected and the current
    /// game activity. These values are edge-triggered because polling account
    /// storage or publishing presence on every redraw would both be needless.
    pub(super) fn sync(&mut self, account: Option<FriendsAccount>, activity: PresenceStatus) {
        if self.account != account {
            self.account = account.clone();
            // Do not leave an old account visible while its worker is accepting
            // the selection command. Late views are additionally filtered below.
            self.view = FriendsView {
                account,
                ..FriendsView::default()
            };
            self.worker.submit(FriendsCommand::Select(self.account.clone()));
        }
        if self.activity != Some(activity) {
            self.activity = Some(activity);
            self.worker.submit(FriendsCommand::Activity(activity));
        }
        self.worker.tick();
        self.pump();
    }

    /// The menu/render boundary reads this value; it cannot obtain a session or
    /// service handle from here.
    #[must_use]
    pub(super) fn view(&self) -> &FriendsView {
        &self.view
    }

    pub(super) fn request_refresh(&mut self) {
        self.worker.submit(FriendsCommand::Refresh);
    }

    pub(super) fn set_overlay_open(&mut self, open: bool) {
        self.worker.submit(FriendsCommand::Overlay(open));
    }

    pub(super) fn mutate(&mut self, mutation: FriendMutation) {
        self.worker.submit(FriendsCommand::Mutate(mutation));
    }

    pub(super) fn set_preferences(&mut self, preferences: FriendsPreferences) {
        self.worker.submit(FriendsCommand::Preferences(preferences));
    }

    pub(super) fn shutdown(&mut self) {
        self.worker.shutdown();
        self.account = None;
        self.activity = None;
        self.view = FriendsView::default();
    }

    fn pump(&mut self) {
        while let Some(view) = self.worker.try_view() {
            if view.account.as_ref().map(|account| account.profile_id)
                == self.account.as_ref().map(|account| account.profile_id)
            {
                self.view = view;
            }
        }
    }
}

impl WindowApp {
    /// The single app-side consumer of account selection and activity. Account
    /// metadata comes from the switcher's in-memory roster rather than a second
    /// disk read, so a selection change reaches Friends in the same frame.
    pub(super) fn drive_friends(&mut self) {
        let account = self
            .nav
            .accounts()
            .ordered()
            .into_iter()
            .find(|account| self.nav.accounts().is_selected(account.profile_id))
            .map(|account| FriendsAccount {
                profile_id: account.profile_id,
                display_name: account.username,
            });
        self.friends.sync(account, friends_activity(&self.ui, self.sim.session_phase()));
    }

    /// The one menu-facing Friends read. Its return type makes bearer-token
    /// access unavailable to any renderer or menu caller.
    #[must_use]
    pub(super) fn friends_view(&self) -> &FriendsView {
        self.friends.view()
    }
}

fn friends_activity(
    ui: &crate::menu::UiState,
    phase: crate::sim::SessionPhase,
) -> PresenceStatus {
    if phase != crate::sim::SessionPhase::Connected {
        return PresenceStatus::Online;
    }
    match ui.kind() {
        Some(crate::menu::SessionKind::Singleplayer) => PresenceStatus::LocalWorld,
        Some(crate::menu::SessionKind::Multiplayer) => PresenceStatus::Server,
        None => PresenceStatus::Online,
    }
}

/// A command crossing from the frame to the private executor. It deliberately
/// has no `Session` or service response variant.
#[derive(Clone, Debug)]
enum FriendsCommand {
    Select(Option<FriendsAccount>),
    Activity(PresenceStatus),
    Overlay(bool),
    Refresh,
    Mutate(FriendMutation),
    Preferences(FriendsPreferences),
    Shutdown,
}

fn apply_command(runtime: &mut FriendsRuntime<SystemFriendsClock>, command: FriendsCommand) -> bool {
    match command {
        FriendsCommand::Select(account) => runtime.select_account(account),
        FriendsCommand::Activity(status) => runtime.set_desired_presence(status),
        FriendsCommand::Overlay(open) => runtime.set_overlay_open(open),
        FriendsCommand::Refresh => runtime.request_refresh(),
        FriendsCommand::Mutate(mutation) => runtime.queue_mutation(mutation),
        FriendsCommand::Preferences(preferences) => runtime.set_preferences(preferences),
        FriendsCommand::Shutdown => {
            runtime.shutdown();
            return false;
        }
    }
    true
}

/// Applies a completion while the worker still owns its session. The only
/// caller-facing product is `FriendsView`, sent after this function returns.
fn apply_completion<C: FriendsClock>(
    runtime: &mut FriendsRuntime<C>,
    completion: FriendsCompletion,
) {
    match completion {
        FriendsCompletion::Resolution { profile_id, session } => {
            runtime.complete_resolution(profile_id, session);
        }
        FriendsCompletion::Service { operation, result } => runtime.complete(&operation, result),
    }
}

enum FriendsCompletion {
    Resolution {
        profile_id: uuid::Uuid,
        session: Option<lodestone_auth::Session>,
    },
    Service {
        operation: FriendsOperation,
        result: Result<FriendsResponse, FriendsServiceError>,
    },
}

#[cfg(not(target_arch = "wasm32"))]
async fn execute(
    service: Option<&FriendsService>,
    resolver: &reqwest::Client,
    runtime: &mut FriendsRuntime<SystemFriendsClock>,
    operation: FriendsOperation,
) {
    if let FriendsOperation::ResolveSession { profile_id } = operation {
        let session = match lodestone_auth::resolve_selected_account(resolver).await {
            lodestone_auth::SelectedAccount::Online(session) if session.profile.id == profile_id => {
                Some(session)
            }
            _ => None,
        };
        apply_completion(runtime, FriendsCompletion::Resolution { profile_id, session });
        return;
    }

    let result = match (service, runtime.session(&operation).cloned()) {
        (Some(service), Some(session)) => execute_service(service, &session, &operation).await,
        _ => Err(FriendsServiceError::Unavailable { retry_after: None }),
    };
    apply_completion(runtime, FriendsCompletion::Service { operation, result });
}

async fn execute_service(
    service: &FriendsService,
    session: &lodestone_auth::Session,
    operation: &FriendsOperation,
) -> Result<FriendsResponse, FriendsServiceError> {
    match operation {
        FriendsOperation::ResolveSession { .. } => unreachable!("resolution does not call FriendsService"),
        FriendsOperation::FetchAttributes => service.get_attributes(session).await.map(FriendsResponse::Attributes),
        FriendsOperation::FetchFriends { entity_tag } => service
            .get_friends(session, entity_tag.as_ref())
            .await
            .map(FriendsResponse::Friends),
        FriendsOperation::PublishPresence { status, entity_tag } => service
            .publish_presence(session, *status, entity_tag.as_ref())
            .await
            .map(FriendsResponse::Presence),
        FriendsOperation::Mutate { mutation } => service
            .mutate_friend(session, mutation.clone())
            .await
            .map(FriendsResponse::Mutation),
        FriendsOperation::SetPreferences { preferences } => service
            .set_preferences(session, *preferences)
            .await
            .map(FriendsResponse::Preferences),
    }
}

#[cfg(not(target_arch = "wasm32"))]
struct NativeFriendsWorker {
    commands: crossbeam_channel::Sender<FriendsCommand>,
    views: crossbeam_channel::Receiver<FriendsView>,
}

#[cfg(not(target_arch = "wasm32"))]
impl NativeFriendsWorker {
    fn new() -> Self {
        let (commands, command_rx) = crossbeam_channel::unbounded();
        let (view_tx, views) = crossbeam_channel::unbounded();
        std::thread::Builder::new()
            .name("lodestone-friends".to_owned())
            .spawn(move || run_native_worker(command_rx, view_tx))
            .expect("creating the Friends worker thread must succeed");
        Self { commands, views }
    }

    fn submit(&self, command: FriendsCommand) {
        let _ = self.commands.send(command);
    }

    fn try_view(&self) -> Option<FriendsView> {
        self.views.try_iter().last()
    }

    fn tick(&self) {}

    fn shutdown(&self) {
        self.submit(FriendsCommand::Shutdown);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn run_native_worker(
    commands: crossbeam_channel::Receiver<FriendsCommand>,
    views: crossbeam_channel::Sender<FriendsView>,
) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("creating the Friends worker runtime must succeed");
    let service = FriendsService::production().ok();
    let resolver = reqwest::Client::new();
    let mut friends = FriendsRuntime::new(SystemFriendsClock::new());

    loop {
        let mut keep_running = true;
        match commands.recv_timeout(Duration::from_millis(100)) {
            Ok(command) => keep_running = apply_command(&mut friends, command),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
        while keep_running {
            match commands.try_recv() {
                Ok(command) => keep_running = apply_command(&mut friends, command),
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    keep_running = false;
                    break;
                }
            }
        }
        if !keep_running {
            break;
        }

        if let Some(operation) = friends.poll() {
            // The resolving/fetching state is observable while a real service
            // request is pending; the subsequent completion replaces it.
            let _ = views.send(friends.view());
            runtime.block_on(execute(service.as_ref(), &resolver, &mut friends, operation));
            let _ = views.send(friends.view());
        }
    }
}

#[cfg(target_arch = "wasm32")]
struct LocalFriendsWorker {
    state: std::rc::Rc<std::cell::RefCell<LocalFriendsState>>,
    views: std::rc::Rc<std::cell::RefCell<Vec<FriendsView>>>,
}

#[cfg(target_arch = "wasm32")]
struct LocalFriendsState {
    runtime: FriendsRuntime<SystemFriendsClock>,
    service: Option<std::rc::Rc<FriendsService>>,
    resolver: reqwest::Client,
}

#[cfg(target_arch = "wasm32")]
impl LocalFriendsWorker {
    fn new() -> Self {
        Self {
            state: std::rc::Rc::new(std::cell::RefCell::new(LocalFriendsState {
                runtime: FriendsRuntime::new(SystemFriendsClock::new()),
                service: FriendsService::production().ok().map(std::rc::Rc::new),
                resolver: reqwest::Client::new(),
            })),
            views: std::rc::Rc::new(std::cell::RefCell::new(Vec::new())),
        }
    }

    fn submit(&self, command: FriendsCommand) {
        let keep_running = apply_command(&mut self.state.borrow_mut().runtime, command);
        self.publish();
        if keep_running {
            self.drive();
        }
    }

    fn try_view(&self) -> Option<FriendsView> {
        let mut views = self.views.borrow_mut();
        let latest = views.pop();
        views.clear();
        latest
    }

    fn tick(&self) {
        self.drive();
    }

    fn shutdown(&self) {
        self.submit(FriendsCommand::Shutdown);
    }

    fn publish(&self) {
        self.views.borrow_mut().push(self.state.borrow().runtime.view());
    }

    fn drive(&self) {
        let operation = self.state.borrow_mut().runtime.poll();
        let Some(operation) = operation else {
            return;
        };
        self.publish();
        let state = self.state.clone();
        let views = self.views.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let (service, resolver) = {
                let state = state.borrow();
                (state.service.clone(), state.resolver.clone())
            };
            execute_local(service.as_deref(), &resolver, state.clone(), operation).await;
            views.borrow_mut().push(state.borrow().runtime.view());
        });
    }
}

#[cfg(target_arch = "wasm32")]
async fn execute_local(
    service: Option<&FriendsService>,
    resolver: &reqwest::Client,
    state: std::rc::Rc<std::cell::RefCell<LocalFriendsState>>,
    operation: FriendsOperation,
) {
    if let FriendsOperation::ResolveSession { profile_id } = operation {
        let session = match lodestone_auth::resolve_selected_account(resolver).await {
            lodestone_auth::SelectedAccount::Online(session) if session.profile.id == profile_id => {
                Some(session)
            }
            _ => None,
        };
        apply_completion(
            &mut state.borrow_mut().runtime,
            FriendsCompletion::Resolution { profile_id, session },
        );
        return;
    }
    let session = state.borrow().runtime.session(&operation).cloned();
    let result = match (service, session) {
        (Some(service), Some(session)) => execute_service(service, &session, &operation).await,
        _ => Err(FriendsServiceError::Unavailable { retry_after: None }),
    };
    apply_completion(
        &mut state.borrow_mut().runtime,
        FriendsCompletion::Service { operation, result },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Default)]
    struct Clock {
        now: std::rc::Rc<std::cell::Cell<Duration>>,
        unix: std::rc::Rc<std::cell::Cell<u64>>,
    }

    impl Clock {
        fn set(&self, seconds: u64) {
            self.now.set(Duration::from_secs(seconds));
            self.unix.set(seconds);
        }
    }

    impl FriendsClock for Clock {
        fn now(&self) -> Duration {
            self.now.get()
        }

        fn unix_seconds(&self) -> u64 {
            self.unix.get()
        }
    }

    fn account() -> FriendsAccount {
        FriendsAccount {
            profile_id: uuid::Uuid::from_u128(1),
            display_name: "Player".to_owned(),
        }
    }

    #[test]
    fn native_worker_completion_order_leaves_only_a_credential_free_view() {
        let clock = Clock::default();
        clock.set(0);
        let mut runtime = FriendsRuntime::new(clock.clone());
        runtime.select_account(Some(account()));
        let resolve = runtime.poll().expect("selected account resolves first");
        assert!(matches!(resolve, FriendsOperation::ResolveSession { .. }));

        // This is the worker-local completion path. No channel or `FriendsView`
        // contains the session supplied here.
        apply_completion(
            &mut runtime,
            FriendsCompletion::Resolution {
                profile_id: account().profile_id,
                session: Some(lodestone_auth::Session {
                    access_token: "must-not-leave-worker".to_owned(),
                    profile: lodestone_auth::Profile {
                        name: "Player".to_owned(),
                        id: account().profile_id,
                        skin: None,
                    },
                    expires_at: u64::MAX,
                }),
            },
        );
        let attributes = runtime.poll().expect("attributes follow resolution");
        assert_eq!(attributes, FriendsOperation::FetchAttributes);
        apply_completion(
            &mut runtime,
            FriendsCompletion::Service {
                operation: attributes,
                result: Ok(FriendsResponse::Attributes(
                    lodestone_auth::friends::UserFriendsAttributes {
                        preferences: FriendsPreferences {
                            enabled: true,
                            allow_requests: true,
                        },
                    },
                )),
            },
        );
        clock.set(10);
        let fetch = runtime.poll().expect("enabled attributes schedule the list fetch");
        assert!(matches!(fetch, FriendsOperation::FetchFriends { .. }));

        let view = runtime.view();
        assert_eq!(view.account, Some(account()));
        assert!(!format!("{view:?}").contains("must-not-leave-worker"));
    }
}

