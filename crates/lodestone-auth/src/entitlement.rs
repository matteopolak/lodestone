//! **Proof that a locally stored account owns the game** — the value every
//! play path in the shell has to be handed before it can start a session.
//!
//! ## What it is
//!
//! [`Entitlement`] is a token with private fields and exactly one constructor,
//! [`Entitlement::from_metadata`], which answers `Some` only when the account
//! roster ([`crate::metadata::AccountsMetadata`]) holds at least one account.
//! There is no `Default`, no public field, and no `new`: a caller that wants one
//! must have a roster with a real account in it.
//!
//! That is the whole point. A `bool` consulted at one call site is the shape
//! that fails open the moment somebody adds a second entry path — the new path
//! simply does not consult it, and nothing is red. A token that the play verbs
//! *require* cannot be forgotten, because the verb does not typecheck without
//! one.
//!
//! ## Why presence in the roster *is* ownership
//!
//! This is the load-bearing claim and it is worth stating precisely, because
//! the alternative — re-checking with Microsoft on every launch — would break
//! offline play entirely.
//!
//! A row is written to the roster in exactly one place: the account screen
//! folds in the profile a **completed** sign-in chain produced. That chain ends
//! by fetching the account's Minecraft profile, and an account with no profile
//! provisioned — i.e. one that authenticates fine but does not own the game —
//! fails there with [`crate::AuthError::NoMinecraftProfile`] and produces no
//! profile at all. There is no `AccountProfile` to store for such an account,
//! and not merely by convention: the roster is keyed on the **Minecraft profile
//! UUID**, which an account without a profile does not have.
//!
//! So "a row exists" and "that account owned the game when it was added" are
//! the same statement, and this type does not re-derive the check — it reuses
//! the one the sign-in chain already performs. Re-verification happens
//! naturally the next time the player signs in or joins an online-mode server.
//!
//! ## What this is not
//!
//! It is not a security boundary. The roster is a plain JSON file in the user's
//! own data directory, and a user who wants to forge one can. Nothing stored on
//! a machine its owner controls can prevent that; a server-side check is the
//! only thing that can, and that is what online-mode join already does. What
//! this type enforces is that the *client* will not knowingly let someone play
//! without having added an account that owns the game.
//!
//! ## How to change it
//!
//! * **Keep the constructor singular.** Adding a second way to produce an
//!   `Entitlement` is adding a bypass, and it will not look like one.
//! * If a per-account ownership flag is ever wanted (a row written *before* the
//!   profile fetch, say), put it on [`crate::metadata::AccountProfile`] and
//!   filter here — do not add a constructor that takes the flag directly.
//!
//! ## Dependencies
//!
//! [`crate::metadata`] and `uuid`. No network, no keychain, no filesystem: this
//! module is a pure function of a roster the caller already loaded, which is
//! what lets the browser build use it unchanged.

use uuid::Uuid;

use crate::metadata::AccountsMetadata;

/// Proof that at least one locally stored account owns the game.
///
/// See the module docs. Constructed only by [`Self::from_metadata`]; the fields
/// are private so no other module — in this crate or any other — can conjure
/// one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entitlement {
    /// The account that satisfies the gate.
    profile_id: Uuid,
    /// That account's username, carried so a caller can say *which* account
    /// authorised the session without re-reading the roster.
    username: String,
}

impl Entitlement {
    /// `Some` when `metadata` holds at least one account, `None` otherwise.
    ///
    /// The account carried is the **selected** one when the selection names a
    /// row that exists, and otherwise the most recently used row — the same
    /// preference order the account list itself draws in, so the name reported
    /// here is the name the player would expect to see. Neither choice affects
    /// *whether* the gate opens; only one account has to be present.
    #[must_use]
    pub fn from_metadata(metadata: &AccountsMetadata) -> Option<Self> {
        let chosen = metadata
            .selected
            .and_then(|id| metadata.profiles.iter().find(|p| p.profile_id == id))
            .or_else(|| metadata.profiles.iter().max_by_key(|p| p.last_used))?;
        Some(Self {
            profile_id: chosen.profile_id,
            username: chosen.username.clone(),
        })
    }

    /// The profile UUID of the account that satisfied the gate.
    #[must_use]
    pub fn profile_id(&self) -> Uuid {
        self.profile_id
    }

    /// The username of the account that satisfied the gate.
    #[must_use]
    pub fn username(&self) -> &str {
        &self.username
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::AccountProfile;

    fn profile(name: &str, last_used: u64) -> AccountProfile {
        AccountProfile {
            profile_id: Uuid::new_v4(),
            username: name.to_owned(),
            skin_url: None,
            last_used,
        }
    }

    #[test]
    fn an_empty_roster_yields_no_entitlement() {
        assert_eq!(Entitlement::from_metadata(&AccountsMetadata::default()), None);
    }

    #[test]
    fn a_roster_with_a_selection_pointing_at_nothing_still_entitles_on_the_row_it_has() {
        // The half-written-metadata case: `selected` names a UUID with no row.
        // The gate is about *whether an account is present*, so a dangling
        // selection must not close it — a player with one account and a stale
        // selection owns the game exactly as much as one without.
        let only = profile("Alice", 5);
        let meta = AccountsMetadata {
            selected: Some(Uuid::new_v4()),
            profiles: vec![only.clone()],
        };
        let entitlement = Entitlement::from_metadata(&meta).expect("one row must entitle");
        assert_eq!(entitlement.profile_id(), only.profile_id);
        assert_eq!(entitlement.username(), "Alice");
    }

    #[test]
    fn the_selected_account_is_preferred_over_the_most_recently_used_one() {
        // Distinct `last_used` values on purpose, and the selected row is
        // deliberately *not* the most recent one: with equal values, or with
        // the selection pointing at the newest row, both hypotheses agree and
        // the assertion would measure nothing.
        let older = profile("Selected", 1);
        let newer = profile("Newest", 99);
        let meta = AccountsMetadata {
            selected: Some(older.profile_id),
            profiles: vec![newer, older.clone()],
        };
        let entitlement = Entitlement::from_metadata(&meta).expect("two rows must entitle");
        assert_eq!(entitlement.username(), "Selected");
        assert_eq!(entitlement.profile_id(), older.profile_id);
    }

    #[test]
    fn with_no_selection_the_most_recently_used_account_is_carried() {
        let older = profile("Older", 3);
        let newer = profile("Newer", 7);
        let meta = AccountsMetadata {
            selected: None,
            // Deliberately stored oldest-last so "the first row" and "the most
            // recent row" are different answers.
            profiles: vec![newer.clone(), older],
        };
        let entitlement = Entitlement::from_metadata(&meta).expect("two rows must entitle");
        assert_eq!(entitlement.username(), "Newer");
        assert_eq!(entitlement.profile_id(), newer.profile_id);
    }

    #[test]
    fn removing_the_last_account_closes_the_gate_again() {
        let only = profile("Solo", 1);
        let mut meta = AccountsMetadata {
            selected: Some(only.profile_id),
            profiles: vec![only.clone()],
        };
        assert!(Entitlement::from_metadata(&meta).is_some());
        meta.remove(only.profile_id);
        assert_eq!(
            Entitlement::from_metadata(&meta),
            None,
            "an emptied roster must not keep entitling"
        );
    }
}
