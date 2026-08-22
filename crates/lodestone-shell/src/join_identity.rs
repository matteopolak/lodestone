//! **Who the local player is** — the one place a join decides which username
//! and UUID to present, for singleplayer and multiplayer alike.
//!
//! ## What it is
//!
//! A ladder with exactly two rungs, resolved from files and nothing else:
//!
//! 1. the account [`lodestone_auth::AccountsMetadata::selected`] names, if any —
//!    its `username` and `profile_id` straight out of `profiles.json`;
//! 2. otherwise [`crate::offline_identity::OfflineIdentity`] — the persisted,
//!    user-editable "Play offline" name and the UUID derived from it.
//!
//! Every production join goes through [`join_identity`]. The only join that does
//! not is [`crate::net::NetClient::connect_as`], which is a live gate asking for
//! an exact username on purpose, and which passes its own profile in.
//!
//! ## Why it exists
//!
//! Because there were two answers to "who am I" and they disagreed. The account
//! switcher wrote `profiles.json`; `NetClient::open_singleplayer` read
//! `offline.json`; and the *skin* — cached at sign-in as `<data_dir>/skin.png`
//! and published by [`crate::skin_fetch`] — followed the Microsoft account. So a
//! singleplayer session drew the signed-in player's skin above the offline
//! player's name, keyed the server-side player file on the offline UUID, and
//! ignored the selection entirely. The owner's report was exactly that: *"it
//! always uses the cracked account and ignores my selection … but it does render
//! the skin for my microsoft account"*.
//!
//! The fix is not to sync the two. It is that there is now one producer, and the
//! offline identity is a *rung* of it rather than a parallel path.
//!
//! ## Identity is not authentication
//!
//! This module answers only "what name and UUID go in the login-start packet".
//! Whether a session is *proved* to a server is a separate axis, resolved by
//! `lodestone_auth::resolve_selected_account` on the net thread — that one opens
//! the OS keychain and POSTs to Microsoft, so it happens once per remote join and
//! never for singleplayer, which vanilla does not authenticate either
//! (`ServerLoginPacketListenerImpl.handleHello` skips the encryption request for
//! a memory connection — see `docs/accounts.md`).
//!
//! The two cannot drift, because both key off the same
//! [`lodestone_auth::AccountsMetadata::selected`] UUID. When authentication
//! succeeds the session's own profile wins, since it is the same account's name
//! read fresher than `profiles.json`'s copy.
//!
//! ## The UUID consequence, which is real and worth knowing
//!
//! `CLAUDE.md` records that **vanilla** offline mode derives the account UUID
//! from the username and ignores what the client sent. Our integrated server does
//! not: it echoes the presented UUID (`login_uuid = Some(uuid)`) and keys the
//! saved player file on it. So a singleplayer world previously entered under the
//! offline identity has its inventory and position filed under the *offline*
//! UUID, and entering it with a Microsoft account selected finds no save and
//! starts that account fresh. That is the same thing vanilla does when a world is
//! opened by a different account, it is recoverable by deselecting the account,
//! and it is the price of the selection being honoured at all.
//!
//! ## How to change it
//!
//! * **[`resolve`] is the whole decision and it is pure.** Add a rung there, not
//!   at a call site; a second place that answers this question is the defect this
//!   module replaced.
//! * **[`join_identity`] forks on `#[cfg(test)]`** so a unit test never reads the
//!   developer's real `profiles.json` and never joins as their premium account —
//!   a shared premium name across gates is a shared player file, and a dead
//!   player is held on the death screen, which sends no chunks. The fork is
//!   asserted by [`unit_tests_never_join_as_the_selected_account`], not assumed.
//! * **A fallback says so.** [`JoinIdentity::announce`] logs which rung was taken
//!   and why, at `info`, because a wrong-but-plausible username is the failure
//!   that looks like success.
//!
//! ## Dependencies
//!
//! [`lodestone_auth::AccountsMetadata`] (a plain JSON read — no keychain, no
//! network), [`crate::offline_identity`], and `lodestone_model`'s
//! [`LoginProfile`], which is what `crate::net` hands the client builder.
//!
//! [`unit_tests_never_join_as_the_selected_account`]: tests::unit_tests_never_join_as_the_selected_account

use lodestone_auth::AccountsMetadata;
use lodestone_client::LoginProfile;

use crate::offline_identity::OfflineIdentity;

/// Which rung of the ladder a join landed on, carrying the profile it produced.
///
/// The rung is part of the value rather than a separate `bool` because the only
/// two consumers are a log line and a test, and both want to say *why* — "joined
/// as Steve" is not a report, "joined as Steve because no account is selected"
/// is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinIdentity {
    /// The account the switcher has selected, as `profiles.json` records it.
    SelectedAccount(LoginProfile),
    /// No account is selected, so the persisted "Play offline" identity.
    Offline(LoginProfile),
    /// An account *is* selected and `profiles.json` has no row for it — the
    /// switcher's selection and its list disagree, which is a corrupt or
    /// half-written metadata file rather than a choice the player made.
    ///
    /// Joining offline is the only thing left to do, but it is **not** the same
    /// outcome as [`Self::Offline`] and must not be logged as one: the player
    /// selected an account and is not getting it.
    SelectedAccountMissing {
        /// The UUID `selected` names, with no profile behind it.
        selected: uuid::Uuid,
        /// The offline identity being used instead.
        fallback: LoginProfile,
    },
}

impl JoinIdentity {
    /// The profile to put in the login-start packet.
    #[must_use]
    pub fn profile(&self) -> &LoginProfile {
        match self {
            Self::SelectedAccount(p) | Self::Offline(p) => p,
            Self::SelectedAccountMissing { fallback, .. } => fallback,
        }
    }

    /// [`Self::profile`] by value, for the caller that is done with the rung.
    #[must_use]
    pub fn into_profile(self) -> LoginProfile {
        match self {
            Self::SelectedAccount(p) | Self::Offline(p) => p,
            Self::SelectedAccountMissing { fallback, .. } => fallback,
        }
    }

    /// Say which rung was taken, then hand back the profile.
    ///
    /// Every arm logs, including the ordinary ones: a session that honoured the
    /// selection and one that silently fell back used to look identical from the
    /// outside, and that is precisely the bug this module exists for. The UUID is
    /// included because it is what the server keys the saved player file on, so a
    /// "where did my inventory go" report is answerable from the log alone.
    #[must_use]
    pub fn announce(self) -> LoginProfile {
        match &self {
            Self::SelectedAccount(p) => tracing::info!(
                target: "auth",
                account = %p.username,
                uuid = %p.uuid,
                "joining as the selected Microsoft account"
            ),
            Self::Offline(p) => tracing::info!(
                target: "auth",
                account = %p.username,
                uuid = %p.uuid,
                "no Microsoft account is selected; joining with the offline identity"
            ),
            Self::SelectedAccountMissing { selected, fallback } => tracing::warn!(
                target: "auth",
                %selected,
                account = %fallback.username,
                "the selected account has no entry in profiles.json; \
                 joining with the offline identity instead"
            ),
        }
        self.into_profile()
    }
}

/// The whole decision, with both files injected — the real ones are a
/// developer's actual account metadata and actual offline name, which no test
/// may read (see the module docs' `#[cfg(test)]` note).
///
/// Pure, total, and the only place the ladder is written down.
#[must_use]
pub fn resolve(metadata: &AccountsMetadata, offline: &OfflineIdentity) -> JoinIdentity {
    let Some(selected) = metadata.selected else {
        return JoinIdentity::Offline(offline.login_profile());
    };
    metadata
        .profiles
        .iter()
        .find(|p| p.profile_id == selected)
        .map_or_else(
            || JoinIdentity::SelectedAccountMissing {
                selected,
                fallback: offline.login_profile(),
            },
            |p| {
                JoinIdentity::SelectedAccount(LoginProfile {
                    username: p.username.clone(),
                    uuid: p.profile_id,
                })
            },
        )
}

/// [`resolve`] against the real on-disk pair, logged.
///
/// This is what a production join calls, and the only reader of the real
/// `profiles.json` outside the account switcher itself. It makes **no network
/// call and does not open the keychain**: the username and UUID are both plain
/// fields of `profiles.json`, which is exactly why the metadata was split from
/// the secret store in the first place.
#[cfg(not(test))]
#[must_use]
pub fn join_identity() -> LoginProfile {
    resolve(&AccountsMetadata::load(), &OfflineIdentity::load()).announce()
}

/// The test build's half: **always the offline identity**, never the selected
/// account.
///
/// A `#[cfg(test)]` fork rather than a `cfg!(test)` early return, so the
/// interception is a thing a test can assert rather than a silent skip — the
/// same shape `crate::net::NetClient::production_origin` and
/// `crate::skin_fetch::read_cached_sheet` already take, both for incidents this
/// repo has actually had.
///
/// What it prevents here is not a keychain write but a quieter defect: a unit
/// test that joined as whichever account the developer happens to have selected
/// would make every gate in this crate share one premium player file, and would
/// make the join identity differ between machines — a test whose input is the
/// person running it.
#[cfg(test)]
#[must_use]
pub fn join_identity() -> LoginProfile {
    JoinIdentity::Offline(OfflineIdentity::load().login_profile()).announce()
}

#[cfg(test)]
mod tests {
    use lodestone_auth::AccountProfile;
    use uuid::Uuid;

    use super::*;

    fn offline() -> OfflineIdentity {
        OfflineIdentity::from_username_unchecked("Player".to_owned())
    }

    fn account(username: &str) -> AccountProfile {
        AccountProfile {
            profile_id: Uuid::from_u128(0x1234_5678_9abc_def0_1234_5678_9abc_def0),
            username: username.to_owned(),
            skin_url: None,
            last_used: 0,
        }
    }

    /// The bug, as a gate: a selected account must reach the login profile.
    ///
    /// The fixture is deliberately discriminating on **both** fields — the
    /// username differs from the offline one *and* the UUID is a v4-shaped
    /// literal rather than the name-derived v3 the offline ladder produces — so
    /// the pre-fix behaviour (returning the offline profile) cannot satisfy it by
    /// coincidence on either half. Two adjacent same-typed fields would
    /// transpose without a trace otherwise.
    #[test]
    fn a_selected_account_is_the_join_identity() {
        let selected = account("PremiumName");
        let metadata = AccountsMetadata {
            selected: Some(selected.profile_id),
            profiles: vec![selected.clone()],
        };
        let offline = offline();
        assert_ne!(
            selected.username,
            offline.username(),
            "the fixture must distinguish the two rungs"
        );
        assert_ne!(
            selected.profile_id,
            offline.uuid(),
            "the fixture must distinguish the two rungs by uuid too"
        );

        let resolved = resolve(&metadata, &offline);
        assert_eq!(
            resolved,
            JoinIdentity::SelectedAccount(LoginProfile {
                username: "PremiumName".to_owned(),
                uuid: selected.profile_id,
            }),
            "the account the switcher selected must be the account a join presents"
        );
    }

    /// The negative control: with nothing selected the offline identity is the
    /// answer, unchanged from before this module existed. Without this, "the
    /// selected account wins" is equally consistent with the offline rung having
    /// been deleted.
    #[test]
    fn nothing_selected_falls_back_to_the_offline_identity() {
        let metadata = AccountsMetadata {
            selected: None,
            profiles: vec![account("PremiumName")],
        };
        let offline = offline();
        assert_eq!(
            resolve(&metadata, &offline),
            JoinIdentity::Offline(offline.login_profile()),
            "a player who never signed in must still join under their own offline name"
        );
    }

    /// A selection with no row behind it is its own outcome, not a quiet
    /// `Offline`: the profile is the same, and the *report* is what differs.
    ///
    /// Asserting the variant rather than the profile is the point — a gate on
    /// `profile()` alone would pass with this arm collapsed into `Offline`, and
    /// the player would be told nothing about an account they did select.
    #[test]
    fn a_selection_with_no_profile_row_says_so() {
        let missing = Uuid::from_u128(0xdead_beef);
        let metadata = AccountsMetadata {
            selected: Some(missing),
            profiles: vec![account("PremiumName")],
        };
        let offline = offline();
        let resolved = resolve(&metadata, &offline);
        assert_eq!(
            resolved,
            JoinIdentity::SelectedAccountMissing {
                selected: missing,
                fallback: offline.login_profile(),
            },
            "a dangling selection must be distinguishable from having selected nothing"
        );
        assert_eq!(
            resolved.profile(),
            &offline.login_profile(),
            "and it must still produce a usable identity"
        );
    }

    /// The `#[cfg(test)]` fork above, asserted rather than assumed, so nobody
    /// removes it as dead code and nobody is surprised that a unit-test build
    /// joins offline no matter what `profiles.json` says.
    ///
    /// It cannot check "the selected account was ignored" directly without
    /// reading the developer's real metadata — which is the very thing it exists
    /// to prevent — so it checks the property that follows: the identity a unit
    /// test joins under is the offline one, name-derived (version 3) rather than
    /// the version-4 UUIDs Microsoft issues. `a_selected_account_is_the_join_identity`
    /// covers the production decision the fork bypasses, so the pair cannot both
    /// be satisfied by never resolving anything.
    #[test]
    fn unit_tests_never_join_as_the_selected_account() {
        let profile = join_identity();
        assert_eq!(
            profile,
            OfflineIdentity::load().login_profile(),
            "a unit-test build must join under the offline identity, whatever is selected"
        );
        assert_eq!(
            profile.uuid.get_version_num(),
            3,
            "a name-derived offline uuid is version 3; a Microsoft profile id is not"
        );
    }
}
