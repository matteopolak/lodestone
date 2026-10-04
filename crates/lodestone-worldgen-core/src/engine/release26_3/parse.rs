use std::collections::HashMap;

use serde_json::Value;

use super::{Axis, Binary, BuildContext, BuildError, Node, NodeId, Program, Shift, Unary};
use super::noise::{NoiseParameters, NoiseSampler};
use super::compile::SourceNode;
use super::tables::TableBuilder;
use super::selection::Selection;
use super::spline::{Spline,SplineCurve,SplinePoint,SplineValue};

const MAX_DEPTH: usize = 128;
const MAX_NODES: usize = 65_536;

pub(super) fn parse(root: &Value, resources: &dyn Fn(&str) -> Option<Value>) -> Result<Program, BuildError> {
    parse_roots(std::slice::from_ref(root), resources, None)
}

pub(super) fn parse_with_context(root: &Value, context: &BuildContext<'_>) -> Result<Program, BuildError> {
    parse_roots(std::slice::from_ref(root), context.density_functions, Some(context))
}

pub(super) fn parse_roots(roots: &[Value], resources: &dyn Fn(&str) -> Option<Value>, context: Option<&BuildContext<'_>>) -> Result<Program, BuildError> {
    if roots.is_empty() { return Err(BuildError::new("density", "at least one root is required")); }
    let mut parser = Parser { resources, context, nodes: Vec::new(), heights: Vec::new(), ready: HashMap::new(),
        active: Vec::new(), noises: Vec::new(), noise_ids: HashMap::new(),
        interned:HashMap::new(), cache_ids:HashMap::new(),tables:TableBuilder::default(),spline_points:0,
        islands:None,blended:HashMap::new() };
    let roots=roots.iter().enumerate().map(|(index,root)|parser.node(root,&format!("density[{index}]"),0))
        .collect::<Result<Vec<_>,_>>()?;
    let compiled=super::compile::compile(&parser.nodes,&roots,&parser.tables.tables)?;
    let frames=super::volume::scratch_frames(&compiled.nodes,&compiled.tables);
    let capacities=super::interpolation::scalar_capacity(&compiled.nodes,&compiled.tables)?;
    let scratch_frames=compiled.roots.iter().map(|id|frames[id.0]).max().unwrap_or(0);
    let scalar_capacity=compiled.roots.iter().map(|id|capacities[id.0]).max().unwrap_or(0);
    Ok(Program {nodes:compiled.nodes,roots:compiled.roots,effects:compiled.effects,
        cache_count:parser.cache_ids.len(),noises:parser.noises,islands:parser.islands,
        tables:compiled.tables,scratch_frames,scalar_capacity})
}

struct Parser<'a> {
    resources: &'a dyn Fn(&str) -> Option<Value>,
    context: Option<&'a BuildContext<'a>>,
    nodes: Vec<SourceNode>,
    heights: Vec<usize>,
    ready: HashMap<String, NodeId>,
    active: Vec<String>,
    noises: Vec<NoiseSampler>,
    noise_ids: HashMap<String, usize>,
    interned: HashMap<[u64;10],NodeId>,
    cache_ids: HashMap<NodeId,usize>,
    tables: TableBuilder,
    spline_points: usize,
    islands: Option<super::islands::IslandNoise>,
    blended: HashMap<[u64;5],NodeId>,
}

impl Parser<'_> {
    fn node(&mut self, value: &Value, path: &str, depth: usize) -> Result<NodeId, BuildError> {
        if depth > MAX_DEPTH || self.nodes.len() >= MAX_NODES {
            return Err(BuildError::new(path, "density graph exceeds depth or node budget"));
        }
        if value.is_number() {
            return self.push(Node::Constant(number(value, path)?), path);
        }
        if let Some(name) = value.as_str() {
            let name = resource_id(name, path)?;
            if self.active.contains(&name) {
                return Err(BuildError::new(path, format!("density reference cycle through {name}")));
            }
            if let Some(&id) = self.ready.get(&name) {
                if depth + self.heights[id.0] > MAX_DEPTH {
                    return Err(BuildError::new(path, "expanded density graph exceeds depth budget"));
                }
                return Ok(id);
            }
            let document = (self.resources)(&name)
                .ok_or_else(|| BuildError::new(path, format!("unresolved density_function {name}")))?;
            self.active.push(name.clone());
            let parsed = self.node(&document, &format!("density_function/{name}"), depth + 1);
            self.active.pop();
            let input = parsed?;
            if self.nodes.len()>=MAX_NODES || self.heights[input.0]>=MAX_DEPTH {
                return Err(BuildError::new(path,"density reference exceeds depth or node budget"));
            }
            let id=NodeId(self.nodes.len());
            self.nodes.push(SourceNode::Reference(input));
            self.heights.push(self.heights[input.0]+1);
            self.ready.insert(name, id);
            return Ok(id);
        }
        let name = value["type"].as_str().ok_or_else(|| BuildError::new(path, "density requires type"))?;
        let name = resource_id(name, path)?;
        let ty = name.strip_prefix("minecraft:").unwrap_or(&name);
        let node = match ty {
            "constant" => Node::Constant(number(&value["value"], path)?),
            "blend_alpha" => Node::Context(super::ContextInput::BlendAlpha),
            "blend_offset" => Node::Context(super::ContextInput::BlendOffset),
            "beardifier" => Node::Context(super::ContextInput::StructureDensity),
            "blend_density" => Node::BlendDensity {input:self.child(value,"input",path,depth)?},
            "old_blended_noise" => return self.blended(value,path),
            "end_outer_islands" => {
                let context=self.context.ok_or_else(||BuildError::new(path,"end_outer_islands requires a seeded BuildContext"))?;
                if self.islands.is_none() {self.islands=Some(super::islands::IslandNoise::new(context.seed));}
                Node::EndIslands
            }
            "distance_to_point" => {
                let point=value["point"].as_array().filter(|point|point.len()==3)
                    .ok_or_else(||BuildError::new(path,"distance_to_point requires three integer point coordinates"))?;
                let point=super::BlockContext {x:integer(&point[0],path)?,y:integer(&point[1],path)?,z:integer(&point[2],path)?};
                let metric=match value["metric"].as_str() {
                    Some("euclidean") => super::distance::Metric::Euclidean,
                    Some("euclidean_squared") => super::distance::Metric::Squared,
                    Some("manhattan") => super::distance::Metric::Manhattan,
                    Some("chebyshev") => super::distance::Metric::Chebyshev,
                    _ => return Err(BuildError::new(path,format!("unsupported distance metric {}",value["metric"]))),
                };
                Node::Distance {point,metric}
            }
            "add" | "sub" | "mul" | "div" | "min" | "max" => {
                let operation = match ty {
                    "add" => Binary::Add, "sub" => Binary::Subtract,
                    "mul" => Binary::Multiply, "div" => Binary::Divide,
                    "min" => Binary::Min, _ => Binary::Max,
                };
                Node::Binary { operation, left: self.child(value, "left", path, depth)?, right: self.child(value, "right", path, depth)? }
            }
            "abs" | "square" | "cube" | "half_negative" | "quarter_negative" | "reciprocal" | "negate" | "squeeze" | "sign" => {
                let operation = match ty {
                    "abs" => Unary::Abs, "square" => Unary::Square, "cube" => Unary::Cube,
                    "half_negative" => Unary::HalfNegative, "quarter_negative" => Unary::QuarterNegative,
                    "reciprocal" => Unary::Reciprocal, "negate" => Unary::Negate,
                    "squeeze" => Unary::Squeeze, _ => Unary::Sign,
                };
                Node::Unary { operation, input: self.child(value, "input", path, depth)? }
            }
            "clamp" => {
                let min = number(&value["min"], path)?;
                let max = number(&value["max"], path)?;
                if min > max { return Err(BuildError::new(path, "clamp min exceeds max")); }
                Node::Clamp { input: self.child(value, "input", path, depth)?, min, max }
            }
            "lerp" => Node::Lerp {
                alpha: self.child(value, "alpha", path, depth)?,
                first: self.child(value, "first", path, depth)?,
                second: self.child(value, "second", path, depth)?,
            },
            "range_choice" => {
                let min = number(&value["min_inclusive"], path)?;
                let max = number(&value["max_exclusive"], path)?;
                if min > max { return Err(BuildError::new(path, "range min exceeds max")); }
                Node::RangeChoice {
                    input: self.child(value, "input", path, depth)?, min, max,
                    inside: self.child(value, "when_in_range", path, depth)?,
                    outside: self.child(value, "when_out_of_range", path, depth)?,
                }
            }
            "gradient" => {
                if value.get("tiling").is_some_and(|v| v.as_str() != Some("clamp_to_edge")) {
                    return Err(BuildError::new(path, format!("unsupported gradient tiling {}", value["tiling"])));
                }
                let from = integer(&value["from_coordinate"], path)?;
                let to = integer(&value["to_coordinate"], path)?;
                let delta = to.wrapping_sub(from);
                if delta == 0 { return Err(BuildError::new(path, "gradient requires distinct coordinates")); }
                let start = number(&value["from_value"], path)?;
                let end = number(&value["to_value"], path)?;
                let axis=axis(&value["axis"], path)?;
                let node=Node::Gradient { axis, from,
                    low: from.min(to), high: from.max(to), value: start,
                    factor: (end - start) / delta as f32 };
                return self.push_key(node,[6,axis as u64,from as u64,to as u64,
                    u64::from(start.to_bits()),u64::from(end.to_bits()),0,0,0,0],path);
            }
            "slice" => Node::Slice { axis: axis(&value["axis"], path)?,
                coordinate: integer(&value["coordinate"], path)?, input: self.child(value, "input", path, depth)? },
            "noise" => {
                let sampler = self.noise(&value["noise"], path)?;
                let mut shifts = [None; 3];
                for (index, field) in ["shift_x", "shift_y", "shift_z"].into_iter().enumerate() {
                    if value.get(field).is_some() {
                        let id = self.child(value, field, path, depth)?;
                        if !matches!(self.nodes[id.0], SourceNode::Operation(Node::Constant(v)) if v.to_bits() == 0) { shifts[index] = Some(id); }
                    }
                }
                Node::Noise { sampler, xz_scale: double(&value["xz_scale"], path)?,
                    y_scale: double(&value["y_scale"], path)?, shifts }
            }
            "shift" | "shift_a" | "shift_b" => Node::Shift {
                sampler: self.noise(&value["noise"], path)?,
                mapping: match ty { "shift" => Shift::ThreeDimensional,
                    "shift_a" => Shift::Horizontal, _ => Shift::RotatedHorizontal },
            },
            "interpolated" => {
                let xz = integer(&value["cell_size_xz"],path)?;
                let y = integer(&value["cell_size_y"],path)?;
                if xz <= 0 || y <= 0 { return Err(BuildError::new(path,"interpolated cell sizes must be positive")); }
                Node::Interpolated { input:self.child(value,"input",path,depth)?,xz,y,
                    inverse_xz:1.0 / xz as f32,inverse_y:1.0 / y as f32 }
            }
            "cache" => {
                let input=self.child(value,"input",path,depth)?;
                let next=self.cache_ids.len();
                let slot=*self.cache_ids.entry(input).or_insert(next);
                Node::Cache {input,slot}
            }
            "find_top_surface" => {
                let lower=integer(&value["lower_bound"],path)?;
                let step=integer(&value["cell_height"],path)?;
                if !(-4064..=4062).contains(&lower) || step<=0 {
                    return Err(BuildError::new(path,"find_top_surface needs lower_bound in [-4064,4062] and positive cell_height"));
                }
                Node::FindTopSurface {density:self.child(value,"density",path,depth)?,
                    upper:self.child(value,"upper_bound",path,depth)?,lower,step}
            }
            "interval_select" => {
                let input=self.child(value,"input",path,depth)?;
                let thresholds=value["thresholds"].as_array().ok_or_else(||BuildError::new(path,"interval_select thresholds must be an array"))?
                    .iter().map(|v|number(v,path)).collect::<Result<Vec<_>,_>>()?;
                let functions=value["functions"].as_array().ok_or_else(||BuildError::new(path,"interval_select functions must be an array"))?;
                if functions.len()<2 || functions.len()>MAX_NODES || thresholds.len()!=functions.len()-1
                    || thresholds.windows(2).any(|pair|pair[0].total_cmp(&pair[1]).is_gt()) {
                    return Err(BuildError::new(path,"interval_select needs ordered thresholds and exactly one more function"));
                }
                let functions=functions.iter().enumerate().map(|(index,v)|self.node(v,&format!("{path}.functions[{index}]"),depth+1))
                    .collect::<Result<Vec<_>,_>>()?;
                Node::Selection {table:self.tables.selection(Selection {input,thresholds,functions})}
            }
            "spline" => {
                let mut spline=Spline {coordinates:Vec::new(),curves:Vec::new(),root:SplineValue::Constant(0.0)};
                spline.root=self.spline_value(&value["spline"],&format!("{path}.spline"),depth+1,&mut spline)?;
                if let SplineValue::Constant(value)=spline.root {Node::Constant(value)}
                else {Node::Spline {table:self.tables.spline(spline)}}
            }
            _ => return Err(BuildError::new(path, format!("unsupported 26.3 density discriminator {name}"))),
        };
        self.push(node, path)
    }

    fn child(&mut self, value: &Value, field: &str, path: &str, depth: usize) -> Result<NodeId, BuildError> {
        self.node(&value[field], &format!("{path}.{field}"), depth + 1)
    }

    fn spline_value(&mut self,value:&Value,path:&str,depth:usize,spline:&mut Spline)->Result<SplineValue,BuildError> {
        if depth>MAX_DEPTH {return Err(BuildError::new(path,"spline exceeds depth budget"));}
        if value.is_number() {return Ok(SplineValue::Constant(float(value,path)?));}
        let coordinate=self.child(value,"coordinate",path,depth)?;
        let coordinate=if let Some(index)=spline.coordinates.iter().position(|&id|id==coordinate) {index}
            else {let index=spline.coordinates.len();spline.coordinates.push(coordinate);index};
        let points=value["points"].as_array().filter(|points|!points.is_empty())
            .ok_or_else(||BuildError::new(path,"spline requires nonempty points"))?;
        self.spline_points=self.spline_points.checked_add(points.len()).filter(|&n|n<=MAX_NODES)
            .ok_or_else(||BuildError::new(path,"spline points exceed node budget"))?;
        let mut parsed=Vec::with_capacity(points.len());
        for (index,point) in points.iter().enumerate() {
            let path=format!("{path}.points[{index}]");
            parsed.push(SplinePoint {location:float(&point["location"],&path)?,derivative:float(&point["derivative"],&path)?,
                value:self.spline_value(&point["value"],&path,depth+1,spline)?});
        }
        let id=spline.curves.len();spline.curves.push(SplineCurve {coordinate,points:parsed});
        Ok(SplineValue::Curve(id))
    }

    fn noise(&mut self, value: &Value, path: &str) -> Result<usize, BuildError> {
        let context = self.context.ok_or_else(|| BuildError::new(path, "noise requires a seeded BuildContext"))?;
        let name = value.as_str().ok_or_else(|| BuildError::new(path, "noise requires a named resource"))?;
        let name = resource_id(name, path)?;
        if let Some(&id) = self.noise_ids.get(&name) { return Ok(id); }
        let document = (context.noises)(&name)
            .ok_or_else(|| BuildError::new(path, format!("unresolved noise {name}")))?;
        let sampler = NoiseParameters::parse(&document)
            .and_then(|parameters| parameters.instantiate(context.seed, context.algorithm, &name))
            .map_err(|error| BuildError::new(&format!("noise/{name}"), error.reason))?;
        let id = self.noises.len();
        self.noises.push(sampler);
        self.noise_ids.insert(name, id);
        Ok(id)
    }

    fn blended(&mut self,value:&Value,path:&str)->Result<NodeId,BuildError> {
        use crate::rng::PositionalRandomFactory;
        let context=self.context.ok_or_else(||BuildError::new(path,"old_blended_noise requires a seeded BuildContext"))?;
        let mut parameters=[0.0;5];
        for (index,name) in ["xz_scale","y_scale","xz_factor","y_factor","smear_scale_multiplier"].into_iter().enumerate() {
            parameters[index]=double(&value[name],path)?;
            let range=if index==4 {1.0..=8.0} else {0.001..=1000.0};
            if !range.contains(&parameters[index]) {return Err(BuildError::new(path,format!("{name} outside {range:?}")));}
        }
        let key=parameters.map(f64::to_bits);
        if let Some(&id)=self.blended.get(&key) {return Ok(id);}
        let [xz,y,xz_factor,y_factor,smear]=parameters;
        let xz=684.412*xz;let y=684.412*y;let smear=y*smear;
        let mut random=if context.algorithm.is_legacy() {context.algorithm.new_instance(context.seed)}
            else {context.algorithm.root_positional(context.seed).from_hash_of("minecraft:terrain")};
        let start=self.noises.len();
        self.noises.push(NoiseSampler::smeared(&mut random,16,smear,0.999_984_741_210_937_5));
        self.noises.push(NoiseSampler::smeared(&mut random,16,smear,0.999_984_741_210_937_5));
        self.noises.push(NoiseSampler::smeared(&mut random,8,smear/y_factor,12.75));
        let first=self.push(Node::Noise {sampler:start,xz_scale:xz,y_scale:y,shifts:[None;3]},path)?;
        let second=self.push(Node::Noise {sampler:start+1,xz_scale:xz,y_scale:y,shifts:[None;3]},path)?;
        let main=self.push(Node::Noise {sampler:start+2,xz_scale:xz/xz_factor,y_scale:y/y_factor,shifts:[None;3]},path)?;
        let half=self.push(Node::Constant(0.5),path)?;
        let shifted=self.push(Node::Binary {operation:Binary::Add,left:main,right:half},path)?;
        let alpha=self.push(Node::Clamp {input:shifted,min:0.0,max:1.0},path)?;
        let id=self.push(Node::Lerp {alpha,first,second},path)?;
        self.blended.insert(key,id);
        Ok(id)
    }

    fn push(&mut self, node: Node, path: &str) -> Result<NodeId, BuildError> {
        self.push_key(node,node.key(),path)
    }

    fn push_key(&mut self,node:Node,key:[u64;10],path:&str)->Result<NodeId,BuildError> {
        if let Some(&id)=self.interned.get(&key) {return Ok(id);}
        let height=1+node.children(&self.tables.tables).into_iter().map(|id|self.heights[id.0]).max().unwrap_or(0);
        if height > MAX_DEPTH || self.nodes.len() >= MAX_NODES {
            return Err(BuildError::new(path, "density graph exceeds depth or node budget"));
        }
        let id = NodeId(self.nodes.len());
        self.nodes.push(SourceNode::Operation(node));
        self.heights.push(height);
        self.interned.insert(key,id);
        Ok(id)
    }
}

fn number(value: &Value, path: &str) -> Result<f32, BuildError> {
    let value = value.as_f64().ok_or_else(|| BuildError::new(path, "expected numeric field"))? as f32;
    if value.is_finite() && (-1_000_000.0..=1_000_000.0).contains(&value) {
        Ok(value)
    } else {
        Err(BuildError::new(path, "numeric field exceeds density range [-1000000, 1000000]"))
    }
}

fn float(value:&Value,path:&str)->Result<f32,BuildError> {
    value.as_f64().map(|v|v as f32).ok_or_else(||BuildError::new(path,"expected float field"))
}

fn integer(value: &Value, path: &str) -> Result<i32, BuildError> {
    value.as_i64().and_then(|v| i32::try_from(v).ok()).ok_or_else(|| BuildError::new(path, "expected i32 field"))
}

fn double(value: &Value, path: &str) -> Result<f64, BuildError> {
    value.as_f64().filter(|v| v.is_finite()).ok_or_else(|| BuildError::new(path, "expected finite double field"))
}

fn axis(value: &Value, path: &str) -> Result<Axis, BuildError> {
    match value.as_str() {
        Some("x") => Ok(Axis::X), Some("y") => Ok(Axis::Y), Some("z") => Ok(Axis::Z),
        _ => Err(BuildError::new(path, "axis must be x, y or z")),
    }
}

fn resource_id(value: &str, path: &str) -> Result<String, BuildError> {
    let (namespace, name) = value.split_once(':').unwrap_or(("minecraft", value));
    if namespace.is_empty() || name.is_empty()
        || !namespace.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_.-".contains(&c))
        || !name.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_./-".contains(&c))
    {
        return Err(BuildError::new(path, format!("invalid resource identifier {value:?}")));
    }
    Ok(format!("{namespace}:{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unseeded_generators_and_malformed_documents_fail_before_sampling() {
        for ty in ["noise", "end_outer_islands", "distance_to_point"] {
            assert!(parse(&json!({"type":ty}), &|_| None).unwrap_err().reason.contains(ty));
        }
        assert!(parse(&json!({"type":"modded:add", "left":1, "right":2}), &|_| None).is_err());
        assert!(parse(&json!(1_000_001), &|_| None).unwrap_err().reason.contains("density range"));
    }

    #[test]
    fn recursive_missing_and_deep_references_are_rejected() {
        assert!(parse(&json!("loop"), &|_| Some(json!("loop"))).unwrap_err().reason.contains("cycle"));
        assert!(parse(&json!("missing"), &|_| None).unwrap_err().reason.contains("unresolved"));
        let mut value = json!(1);
        for _ in 0..MAX_DEPTH { value = json!({"type":"abs", "input":value}); }
        assert!(parse(&value, &|_| None).unwrap_err().reason.contains("budget"));
        for cell in [0,-1] {
            assert!(parse(&json!({"type":"interpolated","input":1,"cell_size_xz":cell,"cell_size_y":1}),&|_|None)
                .unwrap_err().reason.contains("positive"));
        }
    }

    #[test]
    fn named_noise_is_resolved_once_and_runtime_does_not_touch_resources() {
        use std::cell::Cell;
        use super::super::{BlockContext, EvalWorkspace};
        let calls = Cell::new(0);
        let noises = |name: &str| {
            assert_eq!(name, "minecraft:test");
            calls.set(calls.get() + 1);
            Some(json!({"base_octave":-3,"octave_count":2,"base_amplitude":0.75}))
        };
        let context = BuildContext { seed:937, algorithm:crate::rng::Algorithm::Legacy,
            density_functions:&|_| None, noises:&noises };
        let noise = json!({"type":"noise","noise":"test","xz_scale":0.375,"y_scale":0.125,
            "shift_x":0.0625});
        let program = Program::parse_with_context(&json!({"type":"add","left":noise,"right":noise}), &context).unwrap();
        assert_eq!(calls.get(), 1);
        assert_eq!(program.noises.len(), 1);
        let mut workspace = EvalWorkspace::new(&program);
        for x in [-177,37,819] { assert!(program.sample(BlockContext {x,y:63,z:-117}, &mut workspace).unwrap().is_finite()); }
        assert_eq!(calls.get(), 1);
    }
}
