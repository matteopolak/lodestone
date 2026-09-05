//! Credential-free Friends menu state and presentation.
//!
//! The menu receives a [`crate::friends_runtime::FriendsView`] copied from the
//! app boundary.  It neither resolves accounts nor retains a session: refresh
//! and relationship changes leave as [`FriendsIntent`] values for the app to
//! hand back to its Friends worker.

use lodestone_auth::friends::{FriendMutation, FriendProfile, FriendsSnapshot};

use crate::friends_runtime::{FriendsError, FriendsView, FriendsViewState};

use super::options::{self, Placement};
use super::render::{Align, MenuFrame, MenuLabel, MenuNotice, MenuRow, Origin, Slot, TabEntryView};
use super::widget;

pub const TITLE: &str = "Friends";
pub const TAB_LABELS: [&str; 2] = ["Friends", "Pending"];
pub const ROW_H: f32 = options::WIDGET_H;
const HEADER_H: f32 = 62.0;
const FOOTER_H: f32 = options::FOOTER_HEIGHT;
const ROW_W: f32 = options::BIG_BUTTON_WIDTH;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FriendsTab {
    Friends,
    Pending,
}

impl FriendsTab {
    fn index(self) -> usize {
        match self {
            Self::Friends => 0,
            Self::Pending => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FriendsIntent {
    Refresh,
    Mutate(FriendMutation),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Control {
    Tab(FriendsTab),
    Entry(usize),
    Refresh,
    Primary,
    Secondary,
    Done,
}

/// Navigation state whose data boundary is [`FriendsView`].
#[derive(Debug, Clone, Default)]
pub struct FriendsNav {
    view: FriendsView,
    tab: FriendsTab,
    selected: usize,
    scroll: f32,
    intents: Vec<FriendsIntent>,
}

impl Default for FriendsTab {
    fn default() -> Self {
        Self::Friends
    }
}

impl FriendsNav {
    pub fn refresh(&mut self, view: FriendsView) {
        self.view = view;
        self.clamp();
    }

    #[must_use]
    pub fn view(&self) -> &FriendsView {
        &self.view
    }

    #[must_use]
    pub fn tab(&self) -> FriendsTab {
        self.tab
    }

    #[must_use]
    pub fn scroll(&self) -> f32 {
        self.scroll
    }

    pub fn reset(&mut self) {
        self.tab = FriendsTab::Friends;
        self.selected = 0;
        self.scroll = 0.0;
    }

    pub fn scroll_by(&mut self, notches: f32, canvas_height: f32) {
        let Some(mut list) = list_spec(self.entries().len(), self.scroll).model(canvas_height) else {
            return;
        };
        list.mouse_scrolled(notches);
        self.scroll = list.scroll();
    }

    pub fn step(&mut self, forward: bool) {
        let controls = self.controls();
        if controls.is_empty() {
            return;
        }
        self.selected = if forward {
            (self.selected + 1) % controls.len()
        } else {
            (self.selected + controls.len() - 1) % controls.len()
        };
        self.scroll_to_selected();
    }

    pub fn hover_row(&mut self, row: usize) {
        let Some(control) = self.visible_controls().get(row).copied() else {
            return;
        };
        if let Some(selected) = self.controls().iter().position(|candidate| *candidate == control) {
            self.selected = selected;
        }
    }

    pub fn click_row(&mut self, row: usize) -> bool {
        let Some(control) = self.visible_controls().get(row).copied() else {
            return false;
        };
        self.hover_row(row);
        self.activate(control)
    }

    pub fn enter(&mut self) -> bool {
        let Some(control) = self.controls().get(self.selected).copied() else {
            return false;
        };
        self.activate(control)
    }

    pub fn take_intents(&mut self) -> Vec<FriendsIntent> {
        std::mem::take(&mut self.intents)
    }

    fn snapshot(&self) -> Option<&FriendsSnapshot> {
        self.view.snapshot.as_ref()
    }

    fn entries(&self) -> Vec<Entry<'_>> {
        let Some(snapshot) = self.snapshot() else {
            return Vec::new();
        };
        match self.tab {
            FriendsTab::Friends => snapshot.friends.iter().map(Entry::Friend).collect(),
            FriendsTab::Pending => snapshot
                .incoming
                .iter()
                .map(Entry::Incoming)
                .chain(snapshot.outgoing.iter().map(Entry::Outgoing))
                .collect(),
        }
    }

    fn controls(&self) -> Vec<Control> {
        let mut controls = vec![Control::Tab(FriendsTab::Friends), Control::Tab(FriendsTab::Pending)];
        controls.extend((0..self.entries().len()).map(Control::Entry));
        if self.view.account.is_some() {
            controls.push(Control::Refresh);
        }
        if self.selected_entry().is_some() {
            controls.push(Control::Primary);
            if self.secondary_label().is_some() {
                controls.push(Control::Secondary);
            }
        }
        controls.push(Control::Done);
        controls
    }

    fn visible_controls(&self) -> Vec<Control> {
        let mut controls = vec![Control::Tab(FriendsTab::Friends), Control::Tab(FriendsTab::Pending)];
        let entries = self.entries();
        if let Some(list) = list_spec(entries.len(), self.scroll)
            .model(crate::config::MIN_SCALED_HEIGHT as f32)
        {
            controls.extend(list.visible_range().map(Control::Entry));
        }
        if self.view.account.is_some() {
            controls.push(Control::Refresh);
        }
        if self.selected_entry().is_some() {
            controls.push(Control::Primary);
            if self.secondary_label().is_some() {
                controls.push(Control::Secondary);
            }
        }
        controls.push(Control::Done);
        controls
    }

    fn selected_entry(&self) -> Option<Entry<'_>> {
        // The two tabs always precede the list entries. Decode the selected
        // entry directly rather than asking `controls`: controls itself asks
        // whether an entry has actions, so routing back through it recurses.
        let index = self.selected.checked_sub(TAB_LABELS.len())?;
        self.entries().into_iter().nth(index)
    }

    fn primary_label(&self) -> Option<&'static str> {
        match self.selected_entry()? {
            Entry::Friend(_) => Some("Remove"),
            Entry::Incoming(_) => Some("Accept"),
            Entry::Outgoing(_) => Some("Cancel"),
        }
    }

    fn secondary_label(&self) -> Option<&'static str> {
        matches!(self.selected_entry(), Some(Entry::Incoming(_))).then_some("Decline")
    }

    fn activate(&mut self, control: Control) -> bool {
        match control {
            Control::Tab(tab) => {
                self.tab = tab;
                self.selected = 0;
                self.scroll = 0.0;
                false
            }
            Control::Entry(_) => false,
            Control::Refresh if self.view.account.is_some() => {
                self.intents.push(FriendsIntent::Refresh);
                false
            }
            Control::Primary => self.queue_selected(false),
            Control::Secondary => self.queue_selected(true),
            Control::Done => true,
            _ => false,
        }
    }

    fn queue_selected(&mut self, secondary: bool) -> bool {
        let mutation = match self.selected_entry() {
            Some(Entry::Friend(profile)) if !secondary => FriendMutation::Remove(profile.profile_id),
            Some(Entry::Incoming(profile)) if secondary => FriendMutation::Decline(profile.profile_id),
            Some(Entry::Incoming(profile)) => FriendMutation::Accept(profile.profile_id),
            Some(Entry::Outgoing(profile)) if !secondary => FriendMutation::Cancel(profile.profile_id),
            _ => return false,
        };
        self.intents.push(FriendsIntent::Mutate(mutation));
        false
    }

    fn clamp(&mut self) {
        let len = self.controls().len();
        self.selected = self.selected.min(len.saturating_sub(1));
        let Some(list) = list_spec(self.entries().len(), self.scroll)
            .model(crate::config::MIN_SCALED_HEIGHT as f32)
        else {
            self.scroll = 0.0;
            return;
        };
        self.scroll = list.scroll();
    }

    fn scroll_to_selected(&mut self) {
        let Some(Control::Entry(index)) = self.controls().get(self.selected).copied() else {
            return;
        };
        let Some(mut list) = list_spec(self.entries().len(), self.scroll)
            .model(crate::config::MIN_SCALED_HEIGHT as f32)
        else {
            return;
        };
        list.scroll_to_entry(index);
        self.scroll = list.scroll();
    }
}

#[derive(Debug, Clone, Copy)]
enum Entry<'a> {
    Friend(&'a FriendProfile),
    Incoming(&'a FriendProfile),
    Outgoing(&'a FriendProfile),
}

impl Entry<'_> {
    fn label(self) -> String {
        match self {
            Self::Friend(profile) => profile.name.clone(),
            Self::Incoming(profile) => format!("{} (incoming)", profile.name),
            Self::Outgoing(profile) => format!("{} (sent)", profile.name),
        }
    }
}

#[must_use]
pub fn list_spec(len: usize, scroll: f32) -> widget::ListSpec {
    widget::ListSpec::uniform(ROW_H, HEADER_H, FOOTER_H, len, ROW_W).at(scroll)
}

#[must_use]
pub fn frame(nav: &FriendsNav) -> MenuFrame<'static> {
    let entries = nav.entries();
    let mut rows = Vec::new();
    for tab in [FriendsTab::Friends, FriendsTab::Pending] {
        rows.push(MenuRow {
            label: TAB_LABELS[tab.index()].to_owned(),
            enabled: true,
            tab: Some(TabEntryView {
                index: tab.index(),
                count: TAB_LABELS.len(),
                selected: tab == nav.tab,
            }),
            ..Default::default()
        });
    }

    let visible = list_spec(entries.len(), nav.scroll)
        .model(crate::config::MIN_SCALED_HEIGHT as f32)
        .map(|list| list.visible_range())
        .unwrap_or(0..0);
    for index in visible {
        let Some(entry) = entries.get(index).copied() else {
            continue;
        };
        let y = HEADER_H + widget::LIST_CONTENT_PADDING + index as f32 * ROW_H - nav.scroll.floor();
        rows.push(MenuRow {
            label: entry.label(),
            enabled: true,
            slot: Some(Slot {
                origin: Origin::ScreenTop,
                dx: -ROW_W * 0.5,
                dy: y,
                w: ROW_W,
                h: ROW_H,
            }),
            ..Default::default()
        });
    }

    let mut footer = Vec::new();
    if nav.view.account.is_some() {
        footer.push("Refresh");
    }
    if let Some(label) = nav.primary_label() {
        footer.push(label);
    }
    if let Some(label) = nav.secondary_label() {
        footer.push(label);
    }
    footer.push("Done");
    let footer_count = footer.len() as u8;
    rows.extend(
        footer
            .into_iter()
            .enumerate()
            .map(|(index, label)| footer_row(label, index as u8, footer_count)),
    );

    let mut labels = Vec::new();
    if let Some(account) = &nav.view.account {
        labels.push(MenuLabel {
            text: account.display_name.clone(),
            origin: Origin::ScreenTop,
            dx: 0.0,
            dy: 30.0,
            align: Align::Centre,
            colour: widget::ACTIVE_LABEL,
            scale: 1.0,
        });
    }
    let notice = notice(nav.view());
    if entries.is_empty() && notice.is_none() {
        labels.push(MenuLabel {
            text: if nav.tab == FriendsTab::Friends {
                "No friends yet.".to_owned()
            } else {
                "No pending requests.".to_owned()
            },
            origin: Origin::ScreenTop,
            dx: 0.0,
            dy: HEADER_H + 20.0,
            align: Align::Centre,
            colour: widget::ACTIVE_LABEL,
            scale: 1.0,
        });
    }

    let selected = nav
        .controls()
        .get(nav.selected)
        .and_then(|control| nav.visible_controls().iter().position(|visible| visible == control))
        .unwrap_or(usize::MAX);
    MenuFrame {
        rows,
        selected,
        vanilla: true,
        labels,
        notice,
        ..Default::default()
    }
}

fn footer_row(label: &str, index: u8, count: u8) -> MenuRow {
    MenuRow {
        label: label.to_owned(),
        enabled: true,
        slot: Some(Slot {
            origin: Origin::Settings(Placement::Footer { index, count }),
            dx: 0.0,
            dy: 0.0,
            w: options::SMALL_BUTTON_WIDTH,
            h: options::WIDGET_H,
        }),
        ..Default::default()
    }
}

fn notice(view: &FriendsView) -> Option<MenuNotice> {
    let text = match view.error {
        Some(FriendsError::Unauthorized | FriendsError::SignedOut) => "Sign in again to use Friends.",
        Some(FriendsError::PrivacyDenied) => "Friends is unavailable for this account.",
        Some(FriendsError::RateLimited) => "Friends is temporarily rate limited.",
        Some(_) => "Friends could not be updated. Try again later.",
        None => match view.state {
            FriendsViewState::Disabled if view.account.is_none() => "Select an online account to use Friends.",
            FriendsViewState::Resolving | FriendsViewState::FetchingAttributes | FriendsViewState::FetchingFriends => {
                "Loading Friends..."
            }
            FriendsViewState::Backoff => "Friends will retry automatically.",
            _ => return None,
        },
    };
    Some(MenuNotice {
        text: text.to_owned(),
        spans: Vec::new(),
        origin: Origin::ScreenTop,
        dx: -140.0,
        dy: HEADER_H + 18.0,
        w: 280.0,
        bottom: FOOTER_H + 10.0,
        colour: widget::ACTIVE_LABEL,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn profile(id: u128, name: &str) -> FriendProfile {
        FriendProfile { profile_id: Uuid::from_u128(id), name: name.to_owned() }
    }

    fn ready(snapshot: FriendsSnapshot) -> FriendsView {
        FriendsView {
            account: Some(crate::friends_runtime::FriendsAccount {
                profile_id: Uuid::from_u128(99),
                display_name: "Owner".to_owned(),
            }),
            state: FriendsViewState::Ready,
            snapshot: Some(snapshot),
            ..FriendsView::default()
        }
    }

    #[test]
    fn pending_actions_emit_only_supported_relationship_mutations() {
        let mut nav = FriendsNav::default();
        nav.refresh(ready(FriendsSnapshot {
            incoming: vec![profile(1, "Alice")],
            outgoing: vec![profile(2, "Bob")],
            ..FriendsSnapshot::default()
        }));
        nav.activate(Control::Tab(FriendsTab::Pending));
        nav.selected = nav.controls().iter().position(|control| *control == Control::Entry(0)).unwrap();
        nav.activate(Control::Primary);
        assert_eq!(nav.take_intents(), vec![FriendsIntent::Mutate(FriendMutation::Accept(Uuid::from_u128(1)))]);

        nav.selected = nav.controls().iter().position(|control| *control == Control::Entry(1)).unwrap();
        nav.activate(Control::Primary);
        assert_eq!(nav.take_intents(), vec![FriendsIntent::Mutate(FriendMutation::Cancel(Uuid::from_u128(2)))]);
    }

    #[test]
    fn disabled_view_names_the_missing_account_without_a_refresh_button() {
        let nav = FriendsNav::default();
        let frame = frame(&nav);
        assert_eq!(frame.rows.last().map(|row| row.label.as_str()), Some("Done"));
        assert!(frame.notice.as_ref().is_some_and(|notice| notice.text.contains("online account")));
    }

    #[test]
    fn scrolling_uses_pixel_offsets_and_keeps_the_selected_entry_visible() {
        let mut nav = FriendsNav::default();
        nav.refresh(ready(FriendsSnapshot {
            friends: (0..20).map(|id| profile(id, "Friend")).collect(),
            ..FriendsSnapshot::default()
        }));
        nav.selected = nav.controls().iter().position(|control| *control == Control::Entry(15)).unwrap();
        nav.scroll_to_selected();
        assert!(nav.scroll() > 0.0);
    }
}
