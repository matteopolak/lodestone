use std::collections::HashMap;

use super::{Axis, BlockContext, BuildError, EvalState, Node, NodeId, Program, SampleVolume};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Shape {
    size: [usize;3],
    step: [i32;3],
    alignment: [u32;3],
}

impl Shape {
    fn len(self) -> Result<usize,BuildError> {
        self.size.into_iter().try_fold(1usize, |n,size| n.checked_mul(size))
            .filter(|&n| n <= isize::MAX as usize / 4)
            .ok_or_else(|| BuildError::new("volume","intermediate volume size overflow"))
    }
}

fn planned_capacity(nodes: &[Node],tables:&super::tables::Tables,id: NodeId, shape: Shape,
    ready: &mut HashMap<(NodeId,Shape),usize>) -> Result<usize,BuildError> {
    if let Some(&capacity) = ready.get(&(id,shape)) { return Ok(capacity); }
    if ready.len() >= 65_536 { return Err(BuildError::new("volume","intermediate shape plan exceeds node budget")); }
    let mut capacity = shape.len()?;
    match nodes[id.0] {
        Node::Slice {axis,coordinate,input} => {
            let axis = match axis {Axis::X=>0,Axis::Y=>1,Axis::Z=>2};
            let mut inner = shape; inner.size[axis]=1; inner.alignment[axis]=coordinate.unsigned_abs();
            capacity = capacity.max(planned_capacity(nodes,tables,input,inner,ready)?);
        }
        Node::Interpolated {input,xz,y,..} => {
            let cell = [xz,y,xz];
            if (0..3).all(|axis| (shape.step[axis]==cell[axis] || shape.size[axis]==1)
                && shape.alignment[axis] % cell[axis] as u32 == 0) {
                capacity = capacity.max(planned_capacity(nodes,tables,input,shape,ready)?);
            } else {
                let mut expanded = shape;
                if shape.step != [1;3] {
                    for axis in 0..3 {
                        expanded.size[axis] = shape.size[axis].checked_mul(shape.step[axis] as usize)
                            .filter(|&n| n <= i32::MAX as usize)
                            .ok_or_else(|| BuildError::new("volume","interpolation block grid exceeds i32 dimensions"))?;
                    }
                    capacity = capacity.max(expanded.len()?);
                }
                let corner_size = std::array::from_fn(|axis|
                    if cell[axis]==1 {expanded.size[axis]}
                    else {(expanded.size[axis]-1).div_ceil(cell[axis] as usize)+2});
                let corners = Shape {size:corner_size,step:cell,alignment:cell.map(|v| v as u32)};
                capacity = capacity.max(planned_capacity(nodes,tables,input,corners,ready)?);
            }
        }
        node => for child in node.children(tables) {
            capacity = capacity.max(planned_capacity(nodes,tables,child,shape,ready)?);
        },
    }
    ready.insert((id,shape),capacity);
    Ok(capacity)
}

pub(super) fn volume_capacity(nodes: &[Node],tables:&super::tables::Tables,root: NodeId, volume: SampleVolume) -> Result<usize,BuildError> {
    planned_capacity(nodes,tables,root,Shape {size:volume.size,step:volume.step,alignment:[1;3]},&mut HashMap::new())
}

pub(super) fn scalar_capacity(nodes: &[Node],tables:&super::tables::Tables) -> Result<Vec<usize>,BuildError> {
    let mut capacities: Vec<usize> = Vec::with_capacity(nodes.len());
    let mut ready = HashMap::new();
    for &node in nodes {
        let mut capacity = node.children(tables).into_iter().map(|id| capacities[id.0]).max().unwrap_or(0);
        if let Node::Spline {table}=node {capacity=capacity.max(tables.splines[table].coordinates.len());}
        if let Node::Interpolated {input,xz,y,..} = node {
            capacity = capacity.max(planned_capacity(nodes,tables,input,Shape {size:[2;3],step:[xz,y,xz],
                alignment:[xz as u32,y as u32,xz as u32]},&mut ready)?);
        }
        capacities.push(capacity);
    }
    Ok(capacities)
}

fn lerp(t:f32,a:f32,b:f32)->f32 { a+t*(b-a) }

impl Program {
    pub(super) fn interpolate_scalar(&self,input:NodeId,xz:i32,y:i32,position:BlockContext,
        state:&mut EvalState<'_>,scratch:&mut[f32],capacity:usize)->Result<f32,BuildError> {
        let remainder = [position.x.rem_euclid(xz),position.y.rem_euclid(y),position.z.rem_euclid(xz)];
        if remainder == [0;3] { return self.evaluate_scalar(input,position,state,scratch,capacity); }
        let volume = SampleVolume::new(BlockContext {x:position.x.wrapping_sub(remainder[0]),
            y:position.y.wrapping_sub(remainder[1]),z:position.z.wrapping_sub(remainder[2])},[2;3],[xz,y,xz])?;
        let mut corners = [0.0;8];
        self.evaluate_volume(input,volume,&mut corners,scratch,capacity,state)?;
        let [fx,fy,fz] = [remainder[0] as f32/xz as f32,remainder[1] as f32/y as f32,remainder[2] as f32/xz as f32];
        Ok(lerp(fz,lerp(fy,lerp(fx,corners[0],corners[2]),lerp(fx,corners[1],corners[3])),
            lerp(fy,lerp(fx,corners[4],corners[6]),lerp(fx,corners[5],corners[7]))))
    }

    pub(super) fn interpolate_volume(&self,input:NodeId,cell:[i32;3],inverse:[f32;3],volume:SampleVolume,
        output:&mut[f32],scratch:&mut[f32],capacity:usize,state:&mut EvalState<'_>)->Result<(),BuildError> {
        let origins = [volume.min.x,volume.min.y,volume.min.z];
        if (0..3).all(|axis| (volume.step[axis]==cell[axis] || volume.size[axis]==1)
            && origins[axis].rem_euclid(cell[axis])==0) {
            return self.evaluate_volume(input,volume,output,scratch,capacity,state);
        }
        if volume.step != [1;3] {
            let size = std::array::from_fn(|axis| volume.size[axis] * volume.step[axis] as usize);
            let expanded = SampleVolume::new(volume.min,size,[1;3])?;
            let (values,tail) = scratch.split_at_mut(capacity);
            let values = &mut values[..expanded.len()];
            self.interpolate_blocks(input,cell,inverse,expanded,values,tail,capacity,state)?;
            for (index,out) in output.iter_mut().enumerate() {
                let indices = volume.indices(index);
                *out = values[expanded.index(std::array::from_fn(|axis| indices[axis]*volume.step[axis] as usize))];
            }
            return Ok(());
        }
        self.interpolate_blocks(input,cell,inverse,volume,output,scratch,capacity,state)
    }

    fn interpolate_blocks(&self,input:NodeId,cell:[i32;3],inverse:[f32;3],volume:SampleVolume,
        output:&mut[f32],scratch:&mut[f32],capacity:usize,state:&mut EvalState<'_>)->Result<(),BuildError> {
        let origins = [volume.min.x,volume.min.y,volume.min.z];
        let first = std::array::from_fn::<_,3,_>(|axis| origins[axis].div_euclid(cell[axis]));
        let last_block = std::array::from_fn::<_,3,_>(|axis| volume.coordinate(axis,volume.size[axis]-1));
        let last = std::array::from_fn::<_,3,_>(|axis| last_block[axis].div_euclid(cell[axis]));
        let cells = std::array::from_fn::<_,3,_>(|axis| last[axis].wrapping_sub(first[axis]).wrapping_add(1));
        if cells.iter().any(|&n|n<=0) {return Err(BuildError::new("volume","interpolation crosses wrapped coordinate range"));}
        let size = std::array::from_fn(|axis| cells[axis] as usize + usize::from(last_block[axis].rem_euclid(cell[axis])!=0));
        let grid = SampleVolume::new(BlockContext {x:first[0].wrapping_mul(cell[0]),
            y:first[1].wrapping_mul(cell[1]),z:first[2].wrapping_mul(cell[2])},size,cell)?;
        let (values,tail) = scratch.split_at_mut(capacity);
        let values = &mut values[..grid.len()];
        self.evaluate_volume(input,grid,values,tail,capacity,state)?;
        for cz in 0..cells[2] as usize { for cx in 0..cells[0] as usize { for cy in 0..cells[1] as usize {
            let lower=[cx,cy,cz];
            let upper=std::array::from_fn::<_,3,_>(|axis|(lower[axis]+1).min(grid.size[axis]-1));
            let corners=std::array::from_fn::<_,8,_>(|i|values[grid.index([
                if i&1==0 {lower[0]} else {upper[0]},if i&2==0 {lower[1]} else {upper[1]},
                if i&4==0 {lower[2]} else {upper[2]}])]);
            let offset=std::array::from_fn::<_,3,_>(|axis|grid.coordinate(axis,lower[axis]).wrapping_sub(origins[axis]));
            let start=offset.map(|v|0.max(v.saturating_neg()));
            let end=std::array::from_fn::<_,3,_>(|axis|cell[axis].min(volume.size[axis] as i32-offset[axis])-1);
            for z in start[2]..=end[2] {
                let fz=z as f32*inverse[2];
                let z00=lerp(fz,corners[0],corners[4]); let z01=lerp(fz,corners[2],corners[6]);
                let z10=lerp(fz,corners[1],corners[5]); let z11=lerp(fz,corners[3],corners[7]);
                for x in start[0]..=end[0] {
                    let fx=x as f32*inverse[0];
                    let low=lerp(fx,z00,z10); let high=lerp(fx,z01,z11);
                    let increment=(high-low)*inverse[1];
                    let mut value=low+increment*start[1] as f32;
                    for y in start[1]..=end[1] {
                        output[volume.index([(offset[0]+x) as usize,(offset[1]+y) as usize,(offset[2]+z) as usize])]=value;
                        value+=increment;
                    }
                }
            }
        } } }
        Ok(())
    }
}
