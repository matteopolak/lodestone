use std::collections::HashMap;

use super::{Axis, BuildError, Node, NodeId, Shift};
use super::tables::{Tables,TableBuilder};

#[derive(Clone, Copy, Debug)]
pub(super) enum SourceNode {
    Operation(Node),
    Reference(NodeId),
}

impl Node {
    pub(super) fn children(self,tables:&Tables) -> Vec<NodeId> {
        match self {
            Self::Selection {table} => return tables.selection_children(table),
            Self::Spline {table} => return tables.splines[table].coordinates.clone(),
            _ => {},
        }
        let children=match self {
            Self::Unary {input,..} | Self::Clamp {input,..} | Self::Slice {input,..}
                | Self::Interpolated {input,..} | Self::Cache {input,..} | Self::BlendDensity {input} => [Some(input),None,None],
            Self::Binary {left,right,..} => [Some(left),Some(right),None],
            Self::FindTopSurface {density,upper,..} => [Some(density),Some(upper),None],
            Self::Lerp {alpha,first,second} => [Some(alpha),Some(first),Some(second)],
            Self::RangeChoice {input,inside,outside,..} => [Some(input),Some(inside),Some(outside)],
            Self::Noise {shifts,..} => shifts,
            _ => [None;3],
        };
        children.into_iter().flatten().collect()
    }

    fn map_children(mut self, mut map: impl FnMut(NodeId) -> Result<NodeId,BuildError>) -> Result<Self,BuildError> {
        match &mut self {
            Self::Unary {input,..} | Self::Clamp {input,..} | Self::Slice {input,..}
                | Self::Interpolated {input,..} | Self::Cache {input,..} | Self::BlendDensity {input} => *input = map(*input)?,
            Self::Binary {left,right,..} => { *left=map(*left)?; *right=map(*right)?; }
            Self::FindTopSurface {density,upper,..} => { *density=map(*density)?; *upper=map(*upper)?; }
            Self::Lerp {alpha,first,second} => { *alpha=map(*alpha)?; *first=map(*first)?; *second=map(*second)?; }
            Self::RangeChoice {input,inside,outside,..} => { *input=map(*input)?; *inside=map(*inside)?; *outside=map(*outside)?; }
            Self::Noise {shifts,..} => for id in shifts.iter_mut().flatten() { *id=map(*id)?; },
            _ => {},
        }
        Ok(self)
    }

    pub(super) fn axes(self, axes: &[u8],tables:&Tables) -> u8 {
        let inputs = self.children(tables).into_iter().fold(0,|mask,id|mask|axes[id.0]);
        match self {
            Self::Constant(_) => 0,
            Self::Gradient {axis,..} => axis.mask(),
            Self::Slice {axis,..} => inputs & !axis.mask(),
            Self::FindTopSurface {..} => inputs & !2,
            Self::Noise {xz_scale,y_scale,..} => inputs | (if xz_scale==0.0 {0} else {5})
                | (if y_scale==0.0 {0} else {2}),
            Self::Shift {mapping:Shift::ThreeDimensional,..} => 7,
            Self::Shift {..} => 5,
            Self::Context(super::ContextInput::StructureDensity) | Self::Distance {..} => 7,
            Self::Context(_) | Self::EndIslands => 5,
            _ => inputs,
        }
    }

    /// Bitwise load-time identity preserves signed zeros and rounded literals.
    pub(super) fn key(self) -> [u64;10] {
        let mut key=[0;10];
        let words: &[u64] = match self {
            Self::Constant(v) => &[0,u64::from(v.to_bits())],
            Self::Unary {operation,input} => &[1,operation as u64,input.0 as u64],
            Self::Binary {operation,left,right} => &[2,operation as u64,left.0 as u64,right.0 as u64],
            Self::Clamp {input,min,max} => &[3,input.0 as u64,u64::from(min.to_bits()),u64::from(max.to_bits())],
            Self::Lerp {alpha,first,second} => &[4,alpha.0 as u64,first.0 as u64,second.0 as u64],
            Self::RangeChoice {input,min,max,inside,outside} => &[5,input.0 as u64,u64::from(min.to_bits()),
                u64::from(max.to_bits()),inside.0 as u64,outside.0 as u64],
            Self::Gradient {axis,from,low,high,value,factor} => &[6,axis as u64,from as u64,low as u64,
                high as u64,u64::from(value.to_bits()),u64::from(factor.to_bits())],
            Self::Slice {axis,coordinate,input} => &[7,axis as u64,coordinate as u64,input.0 as u64],
            Self::Noise {sampler,xz_scale,y_scale,shifts} => &[8,sampler as u64,xz_scale.to_bits(),y_scale.to_bits(),
                shifts[0].map_or(0,|id|id.0 as u64+1),shifts[1].map_or(0,|id|id.0 as u64+1),shifts[2].map_or(0,|id|id.0 as u64+1)],
            Self::Shift {sampler,mapping} => &[9,sampler as u64,mapping as u64],
            Self::Interpolated {input,xz,y,..} => &[10,input.0 as u64,xz as u64,y as u64],
            Self::Cache {input,slot} => &[11,input.0 as u64,slot as u64],
            Self::FindTopSurface {density,upper,lower,step} => &[12,density.0 as u64,upper.0 as u64,lower as u64,step as u64],
            Self::Selection {table} => &[13,table as u64],
            Self::Spline {table} => &[14,table as u64],
            Self::Context(input) => &[15,input as u64],
            Self::BlendDensity {input} => &[16,input.0 as u64],
            Self::Distance {point,metric} => &[17,point.x as u64,point.y as u64,point.z as u64,metric as u64],
            Self::EndIslands => &[18],
        };
        key[..words.len()].copy_from_slice(words);
        key
    }
}

#[derive(Debug)]
pub(super) struct Compiled {
    pub nodes: Vec<Node>,
    pub roots: Vec<NodeId>,
    pub effects: Vec<bool>,
    pub tables: Tables,
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{BlockContext,EvalWorkspace,Program,SampleVolume};
    use serde_json::json;

    #[test]
    fn uniform_axes_collapse_before_cache_and_broadcast_afterward() {
        let ramp=json!({"type":"gradient","axis":"x","from_coordinate":-10,
            "to_coordinate":10,"from_value":-10,"to_value":10});
        let program=Program::parse(&json!({"type":"cache","input":ramp}),&|_|None).unwrap();
        let mut id=program.roots[0];
        let mut removed=Vec::new();
        while let Node::Slice {axis,input,..}=program.nodes[id.0] {removed.push(axis.mask());id=input;}
        assert_eq!(removed,[2,4]);
        assert!(matches!(program.nodes[id.0],Node::Cache {..}));
        assert!(program.effects[program.roots[0].0]);
        let volume=SampleVolume::new(BlockContext{x:-2,y:19,z:7},[3,2,2],[3,5,7]).unwrap();
        let mut workspace=EvalWorkspace::new(&program);
        workspace.prepare_volume(&program,volume,128).unwrap();
        let mut output=[0.0;12];
        program.sample_volume(volume,&mut output,&mut workspace).unwrap();
        assert_eq!(output,[-2.0,-2.0,1.0,1.0,4.0,4.0,-2.0,-2.0,1.0,1.0,4.0,4.0]);
        assert_eq!(workspace.cache_slots[0].sample(BlockContext{x:1,y:0,z:0}),Some(1.0));
        assert_eq!(workspace.cache_slots[0].sample(BlockContext{x:1,y:19,z:7}),None);
    }

    #[test]
    fn all_roots_share_resources_and_cache_input_identity() {
        let ramp=json!({"type":"gradient","axis":"x","from_coordinate":-10,
            "to_coordinate":10,"from_value":-10,"to_value":10});
        let cached=json!({"type":"cache","input":ramp});
        let roots=[cached.clone(),cached,json!({"type":"cache","input":"ramp"})];
        let lookup=|name:&str|(name=="minecraft:ramp").then(||ramp.clone());
        let program=super::super::parse::parse_roots(&roots,&lookup,None).unwrap();
        assert_eq!(program.cache_count,2);
        assert_eq!(program.roots[0],program.roots[1]);
        assert_ne!(program.roots[0],program.roots[2]);
        let volume=SampleVolume::new(BlockContext{x:3,y:5,z:7},[2,1,1],[2,1,1]).unwrap();
        let mut workspace=EvalWorkspace::new(&program);
        workspace.prepare_request(&program,&[(program.root(0).unwrap(),volume),
            (program.root(2).unwrap(),volume)],64).unwrap();
        let mut output=[0.0;2];
        program.sample_root_volume(program.root(2).unwrap(),volume,&mut output,&mut workspace).unwrap();
        assert_eq!(output,[3.0,5.0]);
        assert!(program.sample_root_volume(program.root(1).unwrap(),volume,&mut output,&mut workspace).is_err());
        assert_eq!(program.sample_root(program.root(0).unwrap(),volume.min(),&mut workspace).unwrap(),3.0);
        workspace.reset_request();
        assert!(workspace.cache_slots.iter().all(|slot|slot.point.is_none()));
    }
}

pub(super) fn compile(source: &[SourceNode], roots: &[NodeId],tables:&Tables) -> Result<Compiled,BuildError> {
    let mut source_axes=Vec::with_capacity(source.len());
    for node in source {
        source_axes.push(match *node {
            SourceNode::Operation(node) => node.axes(&source_axes,tables),
            SourceNode::Reference(input) => source_axes[input.0],
        });
    }
    let mut compiler=Compiler {source,source_axes,nodes:Vec::new(),effects:Vec::new(),
        ready:HashMap::new(),interned:HashMap::new(),source_tables:tables,tables:TableBuilder::default()};
    let roots=roots.iter().map(|&id|compiler.rewrite(id,7)).collect::<Result<Vec<_>,_>>()?;
    Ok(Compiled {nodes:compiler.nodes,roots,effects:compiler.effects,tables:compiler.tables.tables})
}

struct Compiler<'a> {
    source: &'a [SourceNode],
    source_axes: Vec<u8>,
    nodes: Vec<Node>,
    effects: Vec<bool>,
    ready: HashMap<(NodeId,u8),NodeId>,
    interned: HashMap<[u64;10],NodeId>,
    source_tables: &'a Tables,
    tables: TableBuilder,
}

impl Compiler<'_> {
    fn push(&mut self,node:Node)->Result<NodeId,BuildError> {
        let key=node.key();
        if let Some(&id)=self.interned.get(&key) { return Ok(id); }
        if self.nodes.len()>=65_536 {return Err(BuildError::new("density","compiled node budget exceeded"));}
        let effect=matches!(node,Node::Cache {..}|Node::Context(_)|Node::BlendDensity {..})
            || node.children(&self.tables.tables).into_iter().any(|id|self.effects[id.0]);
        let id=NodeId(self.nodes.len());
        self.nodes.push(node); self.effects.push(effect); self.interned.insert(key,id);
        Ok(id)
    }

    fn rewrite(&mut self,id:NodeId,parent_axes:u8)->Result<NodeId,BuildError> {
        if let Some(&result)=self.ready.get(&(id,parent_axes)) {return Ok(result);}
        let result=match self.source[id.0] {
            SourceNode::Reference(input) => self.rewrite(input,parent_axes)?,
            SourceNode::Operation(node @ (Node::Constant(_) | Node::Gradient {..})) => self.push(node)?,
            SourceNode::Operation(node) => {
                let axes=self.source_axes[id.0];
                let mapped=match node {
                    Node::Selection {table} => {
                        let mut selection=self.source_tables.selections[table].clone();
                        selection.input=self.rewrite(selection.input,axes)?;
                        for id in &mut selection.functions {*id=self.rewrite(*id,axes)?;}
                        Node::Selection {table:self.tables.selection(selection)}
                    }
                    Node::Spline {table} => {
                        let mut spline=self.source_tables.splines[table].clone();
                        for id in &mut spline.coordinates {*id=self.rewrite(*id,axes)?;}
                        let mut coordinates=Vec::new();
                        let remap:Vec<_>=spline.coordinates.iter().map(|&id| {
                            if let Some(index)=coordinates.iter().position(|&existing|existing==id) {index}
                            else {let index=coordinates.len();coordinates.push(id);index}
                        }).collect();
                        for curve in &mut spline.curves {curve.coordinate=remap[curve.coordinate];}
                        spline.coordinates=coordinates;
                        Node::Spline {table:self.tables.spline(spline)}
                    }
                    node => node.map_children(|child|self.rewrite(child,
                        if matches!(node,Node::Cache {..}) {7} else {axes}))?,
                };
                let mut result=self.push(mapped)?;
                let mut removed=0;
                let mut inner=result;
                while let Node::Slice {axis,input,..}=self.nodes[inner.0] {
                    removed|=axis.mask(); inner=input;
                }
                let missing=parent_axes & !axes & !removed;
                for axis in [Axis::X,Axis::Z,Axis::Y] {
                    if missing & axis.mask()!=0 {result=self.push(Node::Slice {axis,coordinate:0,input:result})?;}
                }
                result
            }
        };
        self.ready.insert((id,parent_axes),result);
        Ok(result)
    }
}
