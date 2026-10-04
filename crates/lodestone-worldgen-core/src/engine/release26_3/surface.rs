use super::{BlockContext,BuildError,EvalState,NodeId,Program};

impl Program {
    pub(super) fn find_surface(&self,density:NodeId,lower:i32,step:i32,upper:f32,
        mut point:BlockContext,state:&mut EvalState<'_>,scratch:&mut[f32],capacity:usize)->Result<i32,BuildError> {
        let divided=upper/step as f32;
        let truncated=divided as i32;
        let floored=if divided<truncated as f32 {truncated.wrapping_sub(1)} else {truncated};
        let mut y=floored.wrapping_mul(step);
        if y<=lower {return Ok(lower);}
        while y>=lower {
            point.y=y;
            if self.evaluate_scalar(density,point,state,scratch,capacity)?>0.0 {return Ok(y);}
            y=y.wrapping_sub(step);
        }
        Ok(lower)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{EvalWorkspace,SampleVolume};
    use serde_json::json;

    fn ramp(axis:&str)->serde_json::Value {
        json!({"type":"gradient","axis":axis,"from_coordinate":-100,"to_coordinate":100,
            "from_value":-100,"to_value":100})
    }

    #[test]
    fn surface_scan_matches_independent_scalar_and_bulk_witness() {
        let graph=json!({"type":"find_top_surface","lower_bound":-24,"cell_height":8,"upper_bound":17.75,
            "density":{"type":"add","left":{"type":"sub","left":12,"right":ramp("y")},
                "right":{"type":"mul","left":ramp("x"),"right":0.25}}});
        let program=Program::parse(&graph,&|_|None).unwrap();
        let volume=SampleVolume::new(BlockContext::default(),[3,1,2],[10,1,7]).unwrap();
        let mut workspace=EvalWorkspace::new(&program);
        workspace.prepare_volume(&program,volume,256).unwrap();
        let mut output=[0.0;6];
        program.sample_volume(volume,&mut output,&mut workspace).unwrap();
        assert_eq!(output,[8.0,8.0,16.0,8.0,8.0,16.0]);
        let negative=Program::parse(&json!({"type":"find_top_surface","lower_bound":-24,"cell_height":8,
            "upper_bound":-0.25,"density":{"type":"sub","left":-8,"right":ramp("y")}}),&|_|None).unwrap();
        assert_eq!(negative.sample(BlockContext::default(),&mut EvalWorkspace::new(&negative)).unwrap(),-16.0);
        let wrapped=Program::parse(&json!({"type":"interpolated","cell_size_xz":16,"cell_size_y":1,"input":graph}),&|_|None).unwrap();
        let volume=SampleVolume::new(BlockContext{x:1,y:7,z:3},[3,2,2],[1;3]).unwrap();
        let mut workspace=EvalWorkspace::new(&wrapped);
        workspace.prepare_volume(&wrapped,volume,1024).unwrap();
        let mut output=[0.0;12];
        wrapped.sample_volume(volume,&mut output,&mut workspace).unwrap();
        assert_eq!(output,[8.0;12]);
        assert!(wrapped.sample(volume.min(),&mut workspace).unwrap_err().reason.contains("one Y layer"));
        assert_eq!(wrapped.sample(BlockContext::default(),&mut workspace).unwrap(),8.0);
    }

    #[test]
    fn surface_ingress_rejects_invalid_scan_contracts() {
        for (lower,step) in [(-4065,8),(4063,8),(-64,0)] {
            assert!(Program::parse(&json!({"type":"find_top_surface","density":1,"upper_bound":320,
                "lower_bound":lower,"cell_height":step}),&|_|None).unwrap_err().reason.contains("find_top_surface"));
        }
    }
}
