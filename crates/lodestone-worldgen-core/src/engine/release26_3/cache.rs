use super::{BlockContext, BuildError, EvalState, EvalWorkspace, NodeId, Program, SampleVolume};

#[derive(Debug, Default)]
pub(super) struct CacheSlot {
    pub point: Option<(BlockContext,f32)>,
    volume: Option<SampleVolume>,
    values: Vec<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_precedence_nan_and_exact_grid_membership_are_distinct() {
        let volume=SampleVolume::new(BlockContext{x:-3,y:1,z:7},[2,2,1],[3,2,1]).unwrap();
        let mut slot=CacheSlot {point:None,volume:Some(volume),values:vec![11.0,13.0,17.0,19.0]};
        let p=BlockContext{x:0,y:3,z:7};
        assert_eq!(slot.sample(p),Some(19.0));
        assert_eq!(slot.point,None);
        assert_eq!(slot.sample(BlockContext{x:-2,y:1,z:7}),None);
        assert_eq!(slot.sample(BlockContext{x:0,y:4,z:7}),None);
        slot.point=Some((p,23.0));
        assert_eq!(slot.sample(p),Some(23.0));
        slot.point=Some((p,f32::NAN));
        assert_eq!(slot.sample(p),Some(19.0));
        slot.volume=None;
        assert_eq!(slot.sample(p),None);
    }
}

impl CacheSlot {
    pub(super) fn sample(&self,point:BlockContext)->Option<f32> {
        if let Some((key,value))=self.point {
            if key==point && !value.is_nan() {return Some(value);}
        }
        self.volume.and_then(|volume|volume.point_index(point)).map(|index|self.values[index])
    }

    pub(super) fn reserve(&mut self,capacity:usize)->Result<(),BuildError> {
        if capacity>self.values.len() {
            self.values.try_reserve_exact(capacity-self.values.len())
                .map_err(|_|BuildError::new("cache","unable to reserve retained volume"))?;
            self.values.resize(capacity,0.0);
        }
        Ok(())
    }
}

impl EvalWorkspace {
    /// Begins a new immutable world/structure/blending request, retaining storage.
    pub fn reset_request(&mut self) {
        for slot in &mut self.cache_slots {slot.point=None;slot.volume=None;}
        self.memo.fill(super::Memo::default());
        self.epoch=0;
    }
}

impl Program {
    pub(super) fn cache_volume(&self,input:NodeId,slot:usize,volume:SampleVolume,output:&mut[f32],
        scratch:&mut[f32],capacity:usize,state:&mut EvalState<'_>)->Result<(),BuildError> {
        if state.caches[slot].volume!=Some(volume) {
            if state.caches[slot].values.len()<volume.len() {
                return Err(BuildError::new("cache","retained volume requires request preparation"));
            }
            let mut values=std::mem::take(&mut state.caches[slot].values);
            state.caches[slot].volume=None;
            let result=self.evaluate_volume(input,volume,&mut values[..volume.len()],scratch,capacity,state);
            state.caches[slot].values=values;
            result?;
            state.caches[slot].volume=Some(volume);
        }
        output.copy_from_slice(&state.caches[slot].values[..volume.len()]);
        Ok(())
    }
}
