use super::{BuildError,EvalState,NodeId,Program,SampleVolume};

#[derive(Clone,Debug)]
pub(super) struct Selection {
    pub input: NodeId,
    pub thresholds: Vec<f32>,
    pub functions: Vec<NodeId>,
}

impl Selection {
    pub fn index(&self,value:f32)->usize {
        self.thresholds.iter().position(|&threshold|value<threshold).unwrap_or(self.functions.len()-1)
    }
}

impl Program {
    pub(super) fn selection_volume(&self,table:usize,volume:SampleVolume,output:&mut[f32],
        scratch:&mut[f32],capacity:usize,state:&mut EvalState<'_>)->Result<(),BuildError> {
        let selection=&self.tables.selections[table];
        self.evaluate_volume(selection.input,volume,output,scratch,capacity,state)?;
        for (index,&id) in selection.functions.iter().enumerate() {
            let (_,tail)=scratch.split_at_mut(index*capacity);
            let (values,tail)=tail.split_at_mut(capacity);
            self.evaluate_volume(id,volume,&mut values[..volume.len()],tail,capacity,state)?;
        }
        for (index,value) in output.iter_mut().enumerate() {
            *value=scratch[selection.index(*value)*capacity+index];
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{BlockContext,EvalWorkspace};
    use serde_json::json;

    #[test]
    fn threshold_equality_scalar_laziness_and_eager_bulk_match_witness() {
        let selector=json!({"type":"gradient","axis":"x","from_coordinate":-2,"to_coordinate":2,
            "from_value":-1,"to_value":1});
        let functions:Vec<_>=[11,13,15,17].into_iter().map(|v|json!({"type":"cache","input":v})).collect();
        let program=Program::parse(&json!({"type":"interval_select","input":selector,
            "thresholds":[-0.5,0,0],"functions":functions}),&|_|None).unwrap();
        let mut workspace=EvalWorkspace::new(&program);
        assert_eq!(program.sample(BlockContext::default(),&mut workspace).unwrap(),17.0);
        assert!(workspace.cache_slots[..3].iter().all(|slot|slot.point.is_none()));
        assert_eq!(workspace.cache_slots[3].point.unwrap().1,17.0);
        workspace.reset_request();
        let volume=SampleVolume::new(BlockContext{x:-2,y:0,z:0},[5,1,1],[1;3]).unwrap();
        workspace.prepare_volume(&program,volume,512).unwrap();
        let mut output=[0.0;5];
        program.sample_volume(volume,&mut output,&mut workspace).unwrap();
        assert_eq!(output,[11.0,13.0,17.0,17.0,17.0]);
        for (slot,value) in workspace.cache_slots.iter().zip([11.0,13.0,15.0,17.0]) {
            assert_eq!(slot.point,None);
            assert_eq!(slot.sample(BlockContext::default()),Some(value));
        }
        assert_ne!(output[2],13.0);
        let nan=Program::parse(&json!({"type":"interval_select","input":{"type":"sub",
            "left":{"type":"reciprocal","input":0},"right":{"type":"reciprocal","input":0}},
            "thresholds":[0],"functions":[11,17]}),&|_|None).unwrap();
        assert_eq!(nan.sample(BlockContext::default(),&mut EvalWorkspace::new(&nan)).unwrap(),17.0);
    }

    #[test]
    fn interval_ingress_checks_lengths_and_float_order() {
        for (thresholds,functions) in [(json!([]),json!([1])),(json!([0]),json!([1,2,3])),
            (json!([1,0]),json!([1,2,3])),(json!([0.0,-0.0]),json!([1,2,3]))] {
            assert!(Program::parse(&json!({"type":"interval_select","input":0,"thresholds":thresholds,
                "functions":functions}),&|_|None).unwrap_err().reason.contains("interval_select"));
        }
    }
}
