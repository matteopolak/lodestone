use lodestone_model::PredictionSequence;
use lodestone_net::NetError;

use crate::ServerBound;

#[derive(Default)]
pub(super) struct PendingPredictionAck(Option<PredictionSequence>);

impl PendingPredictionAck {
    pub(super) fn observe(&mut self, packet: &ServerBound) -> Result<(), NetError> {
        let sequence = match packet {
            ServerBound::BlockAction { sequence, .. }
            | ServerBound::UseItemOn { sequence, .. } => Some(*sequence),
            ServerBound::UseItem { sequence, .. } => *sequence,
            _ => None,
        };
        let Some(sequence) = sequence else { return Ok(()); };
        let raw = u32::try_from(sequence)
            .map_err(|_| NetError::MalformedFrame("negative block prediction sequence"))?;
        if self.0.is_none_or(|pending| raw > pending.raw()) {
            self.0 = Some(PredictionSequence::new(raw));
        }
        Ok(())
    }

    pub(super) fn take(&mut self) -> Option<PredictionSequence> {
        self.0.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_model::{BlockActionKind, BlockFace, BlockPos, Vec3f};

    #[test]
    fn processed_actions_coalesce_until_the_next_tick() {
        let mut pending = PendingPredictionAck::default();
        pending.observe(&ServerBound::BlockAction {
            action: BlockActionKind::AbortDestroy,
            pos: BlockPos::new(3, 65, -7),
            face: BlockFace::Down,
            sequence: 21,
        }).unwrap();
        for sequence in [Some(13), None, Some(21), Some(8)] {
            pending.observe(&ServerBound::UseItem {
                hand: 0, yaw: 0.0, pitch: 0.0, sequence,
            }).unwrap();
        }
        assert_eq!(pending.take(), Some(PredictionSequence::new(21)));
        assert_eq!(pending.take(), None);
        pending.observe(&ServerBound::UseItemOn {
            pos: BlockPos::new(3, 65, -7),
            face: BlockFace::Up,
            cursor: Vec3f::new(0.25, 1.0, 0.75),
            sequence: 34,
            hand: 0,
        }).unwrap();
        assert_eq!(pending.take(), Some(PredictionSequence::new(34)));
        pending.observe(&ServerBound::ItemDropped { whole_stack: false }).unwrap();
        assert_eq!(pending.take(), None);
        pending.observe(&ServerBound::UseItem {
            hand: 0, yaw: 0.0, pitch: 0.0, sequence: Some(0),
        }).unwrap();
        assert_eq!(pending.take(), Some(PredictionSequence::INITIAL));
    }

    #[test]
    fn invalid_sequence_cannot_replace_the_pending_acknowledgement() {
        let mut pending = PendingPredictionAck::default();
        pending.observe(&ServerBound::UseItem {
            hand: 0, yaw: 0.0, pitch: 0.0, sequence: Some(i32::MAX),
        }).unwrap();
        assert!(pending.observe(&ServerBound::UseItem {
            hand: 0, yaw: 0.0, pitch: 0.0, sequence: Some(-1),
        }).is_err());
        assert_eq!(pending.take(), Some(PredictionSequence::new(i32::MAX as u32)));
    }
}
