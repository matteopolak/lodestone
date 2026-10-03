#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum MeshPriority {
    Background,
    Edit,
}

impl MeshPriority {
    pub(super) const fn index(self) -> usize {
        match self {
            Self::Background => 0,
            Self::Edit => 1,
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct FairMeshOrder {
    consecutive_edits: u8,
}

impl FairMeshOrder {
    pub(super) fn preferred(&self) -> MeshPriority {
        if self.consecutive_edits < 4 {
            MeshPriority::Edit
        } else {
            MeshPriority::Background
        }
    }

    pub(super) fn served(&mut self, priority: MeshPriority) {
        self.consecutive_edits = match priority {
            MeshPriority::Background => 0,
            MeshPriority::Edit => self.consecutive_edits.saturating_add(1).min(4),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mesh_priority_bounds_edit_bursts_and_resets_after_background() {
        let mut order = FairMeshOrder::default();
        let mut actual = Vec::new();
        for _ in 0..12 {
            let priority = order.preferred();
            actual.push(priority);
            order.served(priority);
        }
        use MeshPriority::{Background as B, Edit as E};
        assert_eq!(actual, [E, E, E, E, B, E, E, E, E, B, E, E]);
        assert_ne!(actual, vec![E; 12], "unbounded edit preference fails the order control");
    }

    #[test]
    fn mesh_priority_waits_for_background_after_edits_run_alone() {
        let mut order = FairMeshOrder::default();
        for _ in 0..257 {
            order.served(MeshPriority::Edit);
        }
        assert_eq!(order.preferred(), MeshPriority::Background);
        order.served(MeshPriority::Background);
        assert_eq!(order.preferred(), MeshPriority::Edit);
        assert_eq!(MeshPriority::Background.index(), 0);
        assert_eq!(MeshPriority::Edit.index(), 1);
    }
}
