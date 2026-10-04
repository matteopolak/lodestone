use super::BlockContext;

#[derive(Clone, Copy, Debug)]
pub(super) enum Metric { Euclidean, Squared, Manhattan, Chebyshev }

impl Metric {
    pub(super) fn sample(self,point:BlockContext,position:BlockContext)->f32 {
        let x=point.x.wrapping_sub(position.x) as f32;
        let y=point.y.wrapping_sub(position.y) as f32;
        let z=point.z.wrapping_sub(position.z) as f32;
        match self {
            Self::Euclidean => f64::from((x*x+y*y)+z*z).sqrt() as f32,
            Self::Squared => (x*x+y*y)+z*z,
            Self::Manhattan => (x.abs()+y.abs())+z.abs(),
            Self::Chebyshev => super::java_max(super::java_max(x.abs(),y.abs()),z.abs()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{Program,EvalWorkspace,SampleVolume};
    use serde_json::json;

    #[test]
    fn distance_metrics_use_wrapped_integer_deltas_and_float_products() {
        for (metric,expected) in [("euclidean",13.0),("euclidean_squared",169.0),
            ("manhattan",19.0),("chebyshev",12.0)] {
            let program=Program::parse(&json!({"type":"distance_to_point","point":[3,4,12],"metric":metric}),&|_|None).unwrap();
            let mut workspace=EvalWorkspace::new(&program);
            assert_eq!(program.sample(BlockContext::default(),&mut workspace).unwrap(),expected);
        }
        let program=Program::parse(&json!({"type":"distance_to_point","point":[2147483647,0,0],"metric":"euclidean"}),&|_|None).unwrap();
        let mut workspace=EvalWorkspace::new(&program);
        assert_eq!(program.sample(BlockContext{x:i32::MIN,y:0,z:0},&mut workspace).unwrap(),1.0);
        let program=Program::parse(&json!({"type":"distance_to_point","point":[16777217,16777219,16777221],"metric":"euclidean_squared"}),&|_|None).unwrap();
        let mut workspace=EvalWorkspace::new(&program);
        assert_eq!(program.sample(BlockContext::default(),&mut workspace).unwrap().to_bits(),0x5840_0004);
        let wrong=(16777217.0_f64.powi(2)+16777219.0_f64.powi(2)+16777221.0_f64.powi(2)) as f32;
        assert_eq!(wrong.to_bits(),0x5840_0005);
    }

    #[test]
    fn distance_bulk_obeys_y_fast_stepped_layout_and_rejects_unknown_metrics() {
        let program=Program::parse(&json!({"type":"distance_to_point","point":[1,2,3],"metric":"manhattan"}),&|_|None).unwrap();
        let mut workspace=EvalWorkspace::new(&program);
        let volume=SampleVolume::new(BlockContext{x:-2,y:4,z:-5},[2,2,2],[3,5,7]).unwrap();
        workspace.prepare_volume(&program,volume,0).unwrap();
        let mut output=[0.0;8];
        program.sample_volume(volume,&mut output,&mut workspace).unwrap();
        assert_eq!(output,[13.0,18.0,10.0,15.0,6.0,11.0,3.0,8.0]);
        for document in [json!({"type":"distance_to_point","point":[1,2],"metric":"euclidean"}),
            json!({"type":"distance_to_point","point":[1,2,3],"metric":"taxicab"})] {
            assert!(Program::parse(&document,&|_|None).is_err());
        }
    }
}
