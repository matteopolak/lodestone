//! Checked regular sample volumes. Elements are ordered y, then x, then z.

use super::{Axis, Binary, BlockContext, BuildError, EvalWorkspace, Node, NodeId, Program, RootId, Shift, Unary};
use super::EvalState;

/// A nonempty regular block-coordinate volume with positive integer steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SampleVolume {
    pub(super) min: BlockContext,
    pub(super) size: [usize; 3],
    pub(super) step: [i32; 3],
    len: usize,
}

impl SampleVolume {
    pub fn new(min: BlockContext, size: [usize; 3], step: [i32; 3]) -> Result<Self, BuildError> {
        if size.contains(&0) || size.iter().any(|&n| n > i32::MAX as usize) || step.iter().any(|&n| n <= 0) {
            return Err(BuildError::new("volume", "sizes and steps must be positive i32 values"));
        }
        let len = size.into_iter().try_fold(1usize, |product, n| product.checked_mul(n))
            .filter(|&len| len <= isize::MAX as usize / std::mem::size_of::<f32>())
            .ok_or_else(|| BuildError::new("volume", "element count exceeds addressable float storage"))?;
        Ok(Self { min, size, step, len })
    }

    #[must_use]
    pub fn len(self) -> usize { self.len }

    #[must_use]
    pub fn is_empty(self) -> bool { false }

    #[must_use]
    pub fn size(self) -> [usize; 3] { self.size }

    #[must_use]
    pub fn min(self) -> BlockContext { self.min }

    #[must_use]
    pub fn step(self) -> [i32; 3] { self.step }

    pub(super) fn coordinate(self, axis: usize, index: usize) -> i32 {
        let origin = [self.min.x, self.min.y, self.min.z][axis];
        origin.wrapping_add((index as i32).wrapping_mul(self.step[axis]))
    }

    pub(super) fn indices(self, index: usize) -> [usize;3] {
        [index / self.size[1] % self.size[0], index % self.size[1], index / self.size[1] / self.size[0]]
    }

    pub(super) fn index(self, [x,y,z]: [usize;3]) -> usize { (z * self.size[0] + x) * self.size[1] + y }

    pub(super) fn point_index(self,point:BlockContext)->Option<usize> {
        let position=[point.x,point.y,point.z];
        let origin=[self.min.x,self.min.y,self.min.z];
        let mut indices=[0;3];
        for axis in 0..3 {
            let delta=position[axis].wrapping_sub(origin[axis]);
            if delta<0 || delta>=(self.size[axis] as i32).wrapping_mul(self.step[axis])
                || delta % self.step[axis]!=0 {return None;}
            indices[axis]=(delta/self.step[axis]) as usize;
            if indices[axis]>=self.size[axis] {return None;}
        }
        Some(self.index(indices))
    }

    pub(super) fn check_output(self, output: &[f32]) -> Result<(), BuildError> {
        if output.len() != self.len {
            Err(BuildError::new("volume", format!("expected {} output values, got {}", self.len, output.len())))
        } else { Ok(()) }
    }
}

pub(super) fn scratch_frames(nodes: &[Node],tables:&super::tables::Tables) -> Vec<usize> {
    let mut frames: Vec<usize> = Vec::with_capacity(nodes.len());
    for node in nodes {
        let count = match *node {
            Node::Constant(_) | Node::Gradient { .. } | Node::Context(_) | Node::Distance {..} | Node::EndIslands => 0,
            Node::Unary { input, .. } | Node::Clamp { input, .. } | Node::Cache {input,..} | Node::BlendDensity {input} => frames[input.0],
            Node::Slice { input, .. } => 1 + frames[input.0],
            Node::Interpolated { input, .. } => 2 + frames[input.0],
            Node::Binary { left, right, .. } => frames[left.0].max(1 + frames[right.0]),
            Node::FindTopSurface {density,upper,..} => frames[density.0].max(frames[upper.0]),
            Node::Selection {table} => {
                let table=&tables.selections[table];
                table.functions.iter().enumerate().map(|(index,id)|index+1+frames[id.0])
                    .fold(frames[table.input.0],usize::max)
            }
            Node::Spline {table} => {
                let coordinates=&tables.splines[table].coordinates;
                coordinates.len()+1+coordinates.iter().map(|id|frames[id.0]).max().unwrap_or(0)
            }
            Node::Lerp { alpha, first, second } => frames[alpha.0].max(1 + frames[first.0]).max(2 + frames[second.0]),
            Node::RangeChoice { input, inside, outside, .. } => frames[inside.0].max(1 + frames[input.0]).max(2 + frames[outside.0]),
            Node::Noise { shifts, .. } => shifts.iter().enumerate().filter_map(|(index,id)|
                id.map(|id| index + 1 + frames[id.0])).max().unwrap_or(0).max(if shifts.iter().any(Option::is_some) {3} else {0}),
            Node::Shift { mapping:Shift::RotatedHorizontal, .. } => 1,
            Node::Shift { .. } => 0,
        };
        frames.push(count);
    }
    frames
}

impl EvalWorkspace {
    /// Reserves bulk scratch explicitly. The caller owns the memory budget in
    /// float elements; sampling never grows this allocation or adds caches.
    #[cfg(test)]
    pub fn prepare_volume(&mut self, program: &Program, volume: SampleVolume, scratch_budget: usize) -> Result<(), BuildError> {
        let requests:Vec<_>=(0..program.roots.len()).map(|index|(RootId(index),volume)).collect();
        self.prepare_request(program,&requests,scratch_budget)
    }

    /// Prepares all root/shape consumers together. Budget includes retained
    /// explicit-cache buffers as well as temporary arithmetic/interpolation frames.
    pub fn prepare_request(&mut self,program:&Program,requests:&[(RootId,SampleVolume)],scratch_budget:usize)->Result<(),BuildError> {
        let mut capacity=program.scalar_capacity;
        for &(root,volume) in requests {
            let root=*program.roots.get(root.0).ok_or_else(||BuildError::new("volume","invalid density root"))?;
            capacity=capacity.max(super::interpolation::volume_capacity(&program.nodes,&program.tables,root,volume)?);
        }
        let required = capacity.checked_mul(program.scratch_frames+program.cache_count)
            .filter(|&n| n <= scratch_budget)
            .ok_or_else(|| BuildError::new("volume", "scratch plan exceeds caller budget"))?;
        let required=required-capacity*program.cache_count;
        if required > self.volume_scratch.len() {
            self.volume_scratch.try_reserve_exact(required - self.volume_scratch.len())
                .map_err(|_| BuildError::new("volume", "unable to reserve volume scratch"))?;
            self.volume_scratch.resize(required,0.0);
        }
        self.volume_capacity = capacity;
        self.volume_shapes=requests.iter().map(|&(root,volume)|(root,volume.size,volume.step)).collect();
        for slot in &mut self.cache_slots {slot.reserve(capacity)?;}
        Ok(())
    }
}

impl Program {
    /// Evaluates the bulk contract into y-fast caller-owned output. Branch
    /// buffers follow bulk evaluation order, distinct from lazy scalar queries.
    pub fn sample_volume(&self, volume: SampleVolume, output: &mut [f32], workspace: &mut EvalWorkspace) -> Result<(), BuildError> {
        self.sample_root_volume(RootId(0),volume,output,workspace)
    }

    pub fn sample_root_volume(&self, root:RootId,volume: SampleVolume, output: &mut [f32], workspace: &mut EvalWorkspace) -> Result<(), BuildError> {
        let id=*self.roots.get(root.0).ok_or_else(||BuildError::new("volume","invalid density root"))?;
        volume.check_output(output)?;
        let required = self.scratch_frames.checked_mul(workspace.volume_capacity)
            .ok_or_else(|| BuildError::new("volume", "scratch size overflow"))?;
        if !workspace.volume_shapes.contains(&(root,volume.size,volume.step))
            || volume.len > workspace.volume_capacity || required > workspace.volume_scratch.len() {
            return Err(BuildError::new("volume", "workspace requires prepare_volume for this program and volume"));
        }
        workspace.epoch=workspace.epoch.wrapping_add(1);
        if workspace.epoch==0 {workspace.memo.fill(super::Memo::default());workspace.epoch=1;}
        let mut state=EvalState {memo:&mut workspace.memo,epoch:workspace.epoch,caches:&mut workspace.cache_slots,
            request_context:workspace.request_context.as_deref()};
        self.evaluate_volume(id,volume,output,&mut workspace.volume_scratch,workspace.volume_capacity,&mut state)
    }

    pub(super) fn evaluate_volume(&self, id: NodeId, volume: SampleVolume, output: &mut [f32], scratch: &mut [f32], capacity: usize,state:&mut EvalState<'_>) -> Result<(), BuildError> {
        match self.nodes[id.0] {
            Node::Constant(value) => output.fill(value),
            Node::Context(input) => state.request_context()?.sample_volume(input,volume,output)?,
            Node::BlendDensity {input} => {
                self.evaluate_volume(input,volume,output,scratch,capacity,state)?;
                state.request_context()?.blend_density_volume(volume,output)?;
            }
            Node::Distance {point,metric} => for (index,value) in output.iter_mut().enumerate() {
                let [x,y,z]=volume.indices(index);
                *value=metric.sample(point,BlockContext {x:volume.coordinate(0,x),y:volume.coordinate(1,y),z:volume.coordinate(2,z)});
            },
            Node::EndIslands => self.islands.as_ref().expect("compiled island sampler").fill(volume,output)?,
            Node::Unary { operation,input } => {
                self.evaluate_volume(input,volume,output,scratch,capacity,state)?;
                for value in output {
                    *value = match operation {
                        Unary::Abs => value.abs(), Unary::Square => *value * *value,
                        Unary::Cube => (*value * *value) * *value,
                        Unary::HalfNegative => if *value > 0.0 {*value} else {*value * 0.5},
                        Unary::QuarterNegative => if *value > 0.0 {*value} else {*value * 0.25},
                        Unary::Reciprocal => 1.0 / *value, Unary::Negate => -*value,
                        Unary::Squeeze => { let v = super::java_clamp(*value,-1.0,1.0); v / 2.0 - ((v*v)*v) / 24.0 },
                        Unary::Sign => if *value == 0.0 || value.is_nan() {*value} else {value.signum()},
                    };
                }
            }
            Node::Clamp { input,min,max } => {
                self.evaluate_volume(input,volume,output,scratch,capacity,state)?;
                for value in output { *value = super::java_clamp(*value,min,max); }
            }
            Node::Binary { operation,left,right } => {
                self.evaluate_volume(left,volume,output,scratch,capacity,state)?;
                let (right_output,tail) = scratch.split_at_mut(capacity);
                let right_output = &mut right_output[..volume.len];
                self.evaluate_volume(right,volume,right_output,tail,capacity,state)?;
                for (left,right) in output.iter_mut().zip(right_output) {
                    *left = match operation {
                        Binary::Add => *left + *right, Binary::Subtract => *left - *right,
                        Binary::Multiply => *left * *right, Binary::Divide => *left / *right,
                        Binary::Min => super::java_min(*left,*right), Binary::Max => super::java_max(*left,*right),
                    };
                }
            }
            Node::Lerp { alpha,first,second } => {
                self.evaluate_volume(alpha,volume,output,scratch,capacity,state)?;
                let (first_output,tail) = scratch.split_at_mut(capacity);
                let first_output = &mut first_output[..volume.len];
                self.evaluate_volume(first,volume,first_output,tail,capacity,state)?;
                let (second_output,tail) = tail.split_at_mut(capacity);
                let second_output = &mut second_output[..volume.len];
                self.evaluate_volume(second,volume,second_output,tail,capacity,state)?;
                for ((alpha,first),second) in output.iter_mut().zip(first_output).zip(second_output) {
                    *alpha = if *alpha == 0.0 {*first} else if *alpha == 1.0 {*second}
                        else {*first + *alpha * (*second - *first)};
                }
            }
            Node::RangeChoice { input,min,max,inside,outside } => {
                self.evaluate_volume(inside,volume,output,scratch,capacity,state)?;
                let (selector,tail) = scratch.split_at_mut(capacity);
                let selector = &mut selector[..volume.len];
                self.evaluate_volume(input,volume,selector,tail,capacity,state)?;
                let (other,tail) = tail.split_at_mut(capacity);
                let other = &mut other[..volume.len];
                self.evaluate_volume(outside,volume,other,tail,capacity,state)?;
                for ((value,selector),other) in output.iter_mut().zip(selector).zip(other) {
                    if !(*selector >= min && *selector < max) { *value = *other; }
                }
            }
            Node::Gradient { axis,from,low,high,value,factor } => {
                let axis = match axis { Axis::X=>0, Axis::Y=>1, Axis::Z=>2 };
                for (index,out) in output.iter_mut().enumerate() {
                    let delta = volume.coordinate(axis,volume.indices(index)[axis]).clamp(low,high).wrapping_sub(from);
                    *out = value + delta as f32 * factor;
                }
            }
            Node::Slice { axis,coordinate,input } => {
                let axis_index = match axis { Axis::X=>0, Axis::Y=>1, Axis::Z=>2 };
                let mut size = volume.size; size[axis_index] = 1;
                let sliced = SampleVolume::new(axis.replace(volume.min,coordinate),size,volume.step)?;
                let (values,tail) = scratch.split_at_mut(capacity);
                let values = &mut values[..sliced.len];
                self.evaluate_volume(input,sliced,values,tail,capacity,state)?;
                for (index,out) in output.iter_mut().enumerate() {
                    let mut indices = volume.indices(index); indices[axis_index] = 0;
                    *out = values[sliced.index(indices)];
                }
            }
            Node::Noise { sampler,xz_scale,y_scale,shifts } => {
                if shifts.iter().all(Option::is_none) {
                    output.fill(0.0);
                    self.noises[sampler].add_volume(volume,output,xz_scale,y_scale,1.0)?;
                } else {
                    let (x,tail) = scratch.split_at_mut(capacity);
                    let x = &mut x[..volume.len];
                    if let Some(id) = shifts[0] {self.evaluate_volume(id,volume,x,tail,capacity,state)?} else {x.fill(0.0)}
                    let (y,tail) = tail.split_at_mut(capacity);
                    let y = &mut y[..volume.len];
                    if let Some(id) = shifts[1] {self.evaluate_volume(id,volume,y,tail,capacity,state)?} else {y.fill(0.0)}
                    let (z,tail) = tail.split_at_mut(capacity);
                    let z = &mut z[..volume.len];
                    if let Some(id) = shifts[2] {self.evaluate_volume(id,volume,z,tail,capacity,state)?} else {z.fill(0.0)}
                    for (index,out) in output.iter_mut().enumerate() {
                        let [ix,iy,iz] = volume.indices(index);
                        *out = self.noises[sampler].sample(f64::from(volume.coordinate(0,ix))*xz_scale+f64::from(x[index]),
                            f64::from(volume.coordinate(1,iy))*y_scale+f64::from(y[index]),
                            f64::from(volume.coordinate(2,iz))*xz_scale+f64::from(z[index]));
                    }
                }
            }
            Node::Shift { sampler,mapping } => {
                output.fill(0.0);
                match mapping {
                    Shift::ThreeDimensional | Shift::Horizontal => {
                        let scale_y = if matches!(mapping,Shift::Horizontal) {0.0} else {0.25};
                        self.noises[sampler].add_volume(volume,output,0.25,scale_y,1.0)?;
                        for value in output { *value *= 4.0; }
                    }
                    Shift::RotatedHorizontal => {
                        let mapped = SampleVolume::new(BlockContext {x:volume.min.z,y:volume.min.x,z:0},
                            [volume.size[2],volume.size[0],1],[volume.step[2],volume.step[0],1])?;
                        let values = &mut scratch[..mapped.len]; values.fill(0.0);
                        self.noises[sampler].add_volume(mapped,values,0.25,0.25,4.0)?;
                        for (index,out) in output.iter_mut().enumerate() {
                            let [x,_,z] = volume.indices(index); *out = values[mapped.index([z,x,0])];
                        }
                    }
                }
            }
            Node::Interpolated { input,xz,y,inverse_xz,inverse_y } => {
                self.interpolate_volume(input,[xz,y,xz],[inverse_xz,inverse_y,inverse_xz],volume,output,scratch,capacity,state)?;
            }
            Node::Cache {input,slot} => self.cache_volume(input,slot,volume,output,scratch,capacity,state)?,
            Node::FindTopSurface {density,upper,lower,step} => {
                if volume.size[1]!=1 {return Err(BuildError::new("find_top_surface","bulk input must have one Y layer"));}
                self.evaluate_volume(upper,volume,output,scratch,capacity,state)?;
                for (index,value) in output.iter_mut().enumerate() {
                    let [x,_,z]=volume.indices(index);
                    let point=BlockContext{x:volume.coordinate(0,x),y:volume.min.y,z:volume.coordinate(2,z)};
                    *value=self.find_surface(density,lower,step,*value,point,state,scratch,capacity)? as f32;
                }
            }
            Node::Selection {table} => self.selection_volume(table,volume,output,scratch,capacity,state)?,
            Node::Spline {table} => self.spline_volume(table,volume,output,scratch,capacity,state)?,
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn invalid_shapes_are_rejected_before_sampling() {
        assert!(SampleVolume::new(BlockContext::default(), [0,1,1], [1;3]).is_err());
        assert!(SampleVolume::new(BlockContext::default(), [1;3], [1,0,1]).is_err());
        assert!(SampleVolume::new(BlockContext::default(), [i32::MAX as usize;3], [1;3]).is_err());
        let volume = SampleVolume::new(BlockContext::default(), [2,3,4], [1;3]).unwrap();
        assert_eq!(volume.len(),24);
        assert!(volume.check_output(&[0.0;23]).is_err());
    }

    #[test]
    fn bulk_arithmetic_uses_y_fast_layout_and_reuses_prepared_memory() {
        let program = Program::parse(&json!({"type":"add","left":{"type":"gradient","axis":"x",
            "from_coordinate":-10,"to_coordinate":10,"from_value":-10,"to_value":10},
            "right":{"type":"gradient","axis":"y","from_coordinate":-10,"to_coordinate":10,
                "from_value":-100,"to_value":100}}), &|_|None).unwrap();
        let volume = SampleVolume::new(BlockContext{x:-2,y:1,z:7},[2,3,2],[3,2,7]).unwrap();
        let mut workspace = EvalWorkspace::new(&program);
        let mut output = [0.0;12];
        assert!(program.sample_volume(volume,&mut output,&mut workspace).is_err());
        assert!(workspace.prepare_volume(&program,volume,11).is_err());
        workspace.prepare_volume(&program,volume,24).unwrap();
        let pointer = workspace.volume_scratch.as_ptr();
        program.sample_volume(volume,&mut output,&mut workspace).unwrap();
        assert_eq!(output,[8.0,28.0,48.0,11.0,31.0,51.0,8.0,28.0,48.0,11.0,31.0,51.0]);
        program.sample_volume(volume,&mut output,&mut workspace).unwrap();
        assert_eq!(pointer,workspace.volume_scratch.as_ptr());
    }

    #[test]
    fn bulk_multiply_does_not_inherit_scalar_zero_short_circuit() {
        let ramp = json!({"type":"gradient","axis":"x","from_coordinate":-1,
            "to_coordinate":1,"from_value":-1,"to_value":1});
        let program = Program::parse(&json!({"type":"mul","left":ramp,
            "right":{"type":"reciprocal","input":ramp}}), &|_|None).unwrap();
        let mut workspace = EvalWorkspace::new(&program);
        workspace.prepare_volume(&program,SampleVolume::new(BlockContext::default(),[1;3],[1;3]).unwrap(),3).unwrap();
        let mut output = [0.0];
        program.sample_volume(SampleVolume::new(BlockContext::default(),[1;3],[1;3]).unwrap(),
            &mut output,&mut workspace).unwrap();
        assert!(output[0].is_nan());
        assert_eq!(program.sample(BlockContext::default(),&mut workspace).unwrap().to_bits(),0);
    }
}
