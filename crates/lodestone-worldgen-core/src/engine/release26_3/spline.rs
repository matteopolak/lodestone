use super::{BlockContext,BuildError,EvalState,NodeId,Program,SampleVolume};

#[derive(Clone,Copy,Debug)]
pub(super) enum SplineValue { Constant(f32), Curve(usize) }

#[derive(Clone,Copy,Debug)]
pub(super) struct SplinePoint {
    pub location: f32,
    pub derivative: f32,
    pub value: SplineValue,
}

#[derive(Clone,Debug)]
pub(super) struct SplineCurve {
    pub coordinate: usize,
    pub points: Vec<SplinePoint>,
}

#[derive(Clone,Debug)]
pub(super) struct Spline {
    pub coordinates: Vec<NodeId>,
    pub curves: Vec<SplineCurve>,
    pub root: SplineValue,
}

fn lerp(t:f32,a:f32,b:f32)->f32 {a+t*(b-a)}

impl Spline {
    pub fn sample(&self,value:SplineValue,coordinate:&mut impl FnMut(usize)->Result<f32,BuildError>)->Result<f32,BuildError> {
        let id=match value {SplineValue::Constant(value)=>return Ok(value),SplineValue::Curve(id)=>id};
        let curve=&self.curves[id];
        let x=coordinate(curve.coordinate)?;
        let mut start=0;
        let mut length=curve.points.len();
        while length!=0 {
            let half=length/2;
            let middle=start+half;
            if x<curve.points[middle].location {length=half;} else {start=middle+1;length-=half+1;}
        }
        if start==0 || start==curve.points.len() {
            let point=curve.points[if start==0 {0} else {start-1}];
            let value=self.sample(point.value,coordinate)?;
            return Ok(if point.derivative==0.0 {value} else {value+point.derivative*(x-point.location)});
        }
        let left=curve.points[start-1];
        let right=curve.points[start];
        let t=(x-left.location)/(right.location-left.location);
        let first=self.sample(left.value,coordinate)?;
        let second=self.sample(right.value,coordinate)?;
        let a=left.derivative*(right.location-left.location)-(second-first);
        let b=(-right.derivative)*(right.location-left.location)+(second-first);
        Ok(lerp(t,first,second)+(t*(1.0-t))*lerp(t,a,b))
    }
}

impl Program {
    pub(super) fn spline_scalar(&self,table:usize,point:BlockContext,state:&mut EvalState<'_>,
        scratch:&mut[f32],capacity:usize)->Result<f32,BuildError> {
        let spline=&self.tables.splines[table];
        let (values,tail)=scratch.split_at_mut(capacity);
        values[..spline.coordinates.len()].fill(f32::NAN);
        spline.sample(spline.root,&mut |index| {
            if values[index].is_nan() {
                values[index]=self.evaluate_scalar(spline.coordinates[index],point,state,tail,capacity)?;
            }
            Ok(values[index])
        })
    }

    pub(super) fn spline_volume(&self,table:usize,volume:SampleVolume,output:&mut[f32],
        scratch:&mut[f32],capacity:usize,state:&mut EvalState<'_>)->Result<(),BuildError> {
        let spline=&self.tables.splines[table];
        let (ready,tail)=scratch.split_at_mut(capacity);
        ready[..spline.coordinates.len()].fill(0.0);
        let (buffers,tail)=tail.split_at_mut(spline.coordinates.len()*capacity);
        for (index,value) in output.iter_mut().enumerate() {
            *value=spline.sample(spline.root,&mut |coordinate| {
                let values=&mut buffers[coordinate*capacity..coordinate*capacity+volume.len()];
                if ready[coordinate]==0.0 {
                    self.evaluate_volume(spline.coordinates[coordinate],volume,values,tail,capacity,state)?;
                    ready[coordinate]=1.0;
                }
                Ok(values[index])
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::EvalWorkspace;
    use serde_json::json;

    #[test]
    fn nested_float_arithmetic_matches_independent_bytecode_witness() {
        let spline=Spline {coordinates:vec![NodeId(0),NodeId(1)],root:SplineValue::Curve(1),curves:vec![
            SplineCurve {coordinate:1,points:vec![
                SplinePoint {location:-0.3125,derivative:0.137,value:SplineValue::Constant(0.63125)},
                SplinePoint {location:0.8125,derivative:-0.193,value:SplineValue::Constant(-0.21875)}]},
            SplineCurve {coordinate:0,points:vec![
                SplinePoint {location:-0.7,derivative:0.2137,value:SplineValue::Constant(0.3125)},
                SplinePoint {location:0.65,derivative:-0.173,value:SplineValue::Curve(0)},
                SplinePoint {location:1.125,derivative:0.037,value:SplineValue::Constant(-0.84375)}]}]};
        let positions=[[-0.9,0.1],[-0.5,0.137],[0.137,-0.1875],[0.637,0.25],[0.9,0.73],[1.125,-0.1]];
        let expected=[0x3e8a1dfc,0x3eb4084d,0x3f144257,0x3e8282ee,0xbf0e24e8,0xbf580000];
        for (point,bits) in positions.into_iter().zip(expected) {
            assert_eq!(spline.sample(spline.root,&mut |index|Ok(point[index])).unwrap().to_bits(),bits,"{point:?}");
        }
        assert_ne!(expected[1],0x3eb4084c);
        assert_ne!(expected[2],0x3f144256);
        assert_ne!(expected[3],0x3e8282ef);
    }

    #[test]
    fn nested_coordinates_are_lazy_scalar_and_lazy_whole_volume() {
        let coordinate=|axis|json!({"type":"cache","input":{"type":"gradient","axis":axis,
            "from_coordinate":-10,"to_coordinate":10,"from_value":-10,"to_value":10}});
        let graph=json!({"type":"spline","spline":{"coordinate":coordinate("x"),"points":[
            {"location":0,"derivative":0,"value":7},
            {"location":1,"derivative":0,"value":{"coordinate":coordinate("y"),"points":[
                {"location":0,"derivative":1,"value":11},{"location":10,"derivative":1,"value":21}]}}]}});
        let program=Program::parse(&graph,&|_|None).unwrap();
        let volume=SampleVolume::new(BlockContext{x:-1,y:2,z:7},[3,2,1],[1,3,1]).unwrap();
        let mut workspace=EvalWorkspace::new(&program);
        assert!(program.sample(volume.min(),&mut workspace).is_err());
        workspace.prepare_volume(&program,volume,512).unwrap();
        assert_eq!(program.sample(volume.min(),&mut workspace).unwrap(),7.0);
        assert!(workspace.cache_slots[1].point.is_none());
        workspace.reset_request();
        let pointer=workspace.volume_scratch.as_ptr();
        let mut output=[0.0;6];
        program.sample_volume(volume,&mut output,&mut workspace).unwrap();
        assert_eq!(output,[7.0,7.0,7.0,7.0,13.0,16.0]);
        assert!(workspace.cache_slots.iter().all(|slot|slot.point.is_none()));
        assert_eq!(workspace.cache_slots[1].sample(BlockContext{x:0,y:5,z:0}),Some(5.0));
        program.sample_volume(volume,&mut output,&mut workspace).unwrap();
        assert_eq!(pointer,workspace.volume_scratch.as_ptr());
    }

    #[test]
    fn spline_ingress_rejects_empty_points_without_inventing_sort_constraints() {
        assert!(Program::parse(&json!({"type":"spline","spline":{"coordinate":0,"points":[]}}),&|_|None)
            .unwrap_err().reason.contains("nonempty"));
        for locations in [[1.0,0.0],[0.0,0.0]] {
            assert!(Program::parse(&json!({"type":"spline","spline":{"coordinate":0,"points":[
                {"location":locations[0],"derivative":0,"value":7},
                {"location":locations[1],"derivative":0,"value":11}]}}),&|_|None).is_ok());
        }
    }
}
