//! Typed 32-bit density arithmetic for the 26.3 resource schema.
//!
//! This scalar numeric subset has no terrain fallback. Unsupported
//! operations fail during loading. Sampling only reads numeric arena entries
//! and reusable workspace memory; JSON and resource names stay at ingress.

mod parse;
mod compile;
mod cache;
mod surface;
mod selection;
mod spline;
mod tables;
mod context;
mod distance;
mod islands;
mod interpolation;
pub mod noise;
pub mod volume;

pub use volume::SampleVolume;
pub use context::{ContextInput,EmptyContext,RequestContext};

use serde_json::Value;

/// Load-time resources and the world's random stream configuration.
pub struct BuildContext<'a> {
    pub seed: i64,
    pub algorithm: crate::rng::Algorithm,
    pub density_functions: &'a dyn Fn(&str) -> Option<Value>,
    pub noises: &'a dyn Fn(&str) -> Option<Value>,
}

impl std::fmt::Debug for BuildContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuildContext").field("seed", &self.seed)
            .field("algorithm", &self.algorithm).finish_non_exhaustive()
    }
}

/// Integer block position at which a density graph is sampled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockContext {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

#[derive(Clone, Copy, Debug)]
enum Axis { X, Y, Z }

impl Axis {
    fn mask(self) -> u8 { match self { Self::X => 1, Self::Y => 2, Self::Z => 4 } }
    fn get(self, context: BlockContext) -> i32 {
        match self { Self::X => context.x, Self::Y => context.y, Self::Z => context.z }
    }

    fn replace(self, mut context: BlockContext, coordinate: i32) -> BlockContext {
        match self { Self::X => context.x = coordinate, Self::Y => context.y = coordinate, Self::Z => context.z = coordinate }
        context
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct NodeId(usize);

/// An ordinal root in a compiled multi-root program.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RootId(usize);

#[derive(Clone, Copy, Debug)]
enum Unary { Abs, Square, Cube, HalfNegative, QuarterNegative, Reciprocal, Negate, Squeeze, Sign }

#[derive(Clone, Copy, Debug)]
enum Binary { Add, Subtract, Multiply, Divide, Min, Max }

#[derive(Clone, Copy, Debug)]
enum Shift { ThreeDimensional, Horizontal, RotatedHorizontal }

#[derive(Clone, Copy, Debug)]
enum Node {
    Constant(f32),
    Unary { operation: Unary, input: NodeId },
    Binary { operation: Binary, left: NodeId, right: NodeId },
    Clamp { input: NodeId, min: f32, max: f32 },
    Lerp { alpha: NodeId, first: NodeId, second: NodeId },
    RangeChoice { input: NodeId, min: f32, max: f32, inside: NodeId, outside: NodeId },
    Gradient { axis: Axis, from: i32, low: i32, high: i32, value: f32, factor: f32 },
    Slice { axis: Axis, coordinate: i32, input: NodeId },
    Noise { sampler: usize, xz_scale: f64, y_scale: f64, shifts: [Option<NodeId>; 3] },
    Shift { sampler: usize, mapping: Shift },
    Interpolated { input: NodeId, xz: i32, y: i32, inverse_xz: f32, inverse_y: f32 },
    Cache { input: NodeId, slot: usize },
    FindTopSurface { density: NodeId, upper: NodeId, lower: i32, step: i32 },
    Selection { table: usize },
    Spline { table: usize },
    Context(ContextInput),
    BlendDensity { input: NodeId },
    Distance { point: BlockContext, metric: distance::Metric },
    EndIslands,
}

/// Immutable resolved numeric arena. Named references share node identities.
#[derive(Clone, Debug)]
pub struct Program {
    nodes: Vec<Node>,
    roots: Vec<NodeId>,
    effects: Vec<bool>,
    cache_count: usize,
    noises: Vec<noise::NoiseSampler>,
    islands: Option<islands::IslandNoise>,
    tables: tables::Tables,
    scratch_frames: usize,
    scalar_capacity: usize,
}

/// A rejected document location and its exact unsupported or malformed contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildError {
    pub path: String,
    pub reason: String,
}

impl BuildError {
    fn new(path: &str, reason: impl Into<String>) -> Self {
        Self { path: path.to_owned(), reason: reason.into() }
    }
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.reason)
    }
}

impl std::error::Error for BuildError {}

#[derive(Clone, Copy, Debug, Default)]
struct Memo {
    epoch: u64,
    context: BlockContext,
    value: f32,
}

struct EvalState<'a> {
    memo: &'a mut [Memo],
    epoch: u64,
    caches: &'a mut [cache::CacheSlot],
    request_context: Option<&'a dyn RequestContext>,
}

/// Reusable per-request evaluation memory, allocated once for a program.
/// A node's cached value is valid only at the same position in the same sample.
#[derive(Debug)]
pub struct EvalWorkspace {
    memo: Vec<Memo>,
    epoch: u64,
    volume_scratch: Vec<f32>,
    volume_capacity: usize,
    volume_shapes: Vec<(RootId,[usize;3],[i32;3])>,
    cache_slots: Vec<cache::CacheSlot>,
    request_context: Option<std::sync::Arc<dyn RequestContext>>,
}

impl EvalWorkspace {
    #[must_use]
    pub fn new(program: &Program) -> Self {
        Self { memo: vec![Memo::default(); program.nodes.len()], epoch: 0,
            volume_scratch: Vec::new(), volume_capacity: 0, volume_shapes:Vec::new(),
            cache_slots: (0..program.cache_count).map(|_| cache::CacheSlot::default()).collect(),request_context:None }
    }
}

impl Program {
    /// Resolves references and validates the complete graph before publication.
    /// The lookup receives canonical namespaced density-function resource ids.
    pub fn parse(root: &Value, resources: &dyn Fn(&str) -> Option<Value>) -> Result<Self, BuildError> {
        parse::parse(root, resources)
    }

    /// Resolves named noise parameters and seeds immutable samplers at ingress.
    pub fn parse_with_context(root: &Value, context: &BuildContext<'_>) -> Result<Self, BuildError> {
        parse::parse_with_context(root, context)
    }

    /// Resolves all consumers together, sharing named nodes, noises and cache IDs.
    #[cfg(test)]
    pub fn parse_roots_with_context(roots: &[Value], context: &BuildContext<'_>) -> Result<Self, BuildError> {
        parse::parse_roots(roots, context.density_functions, Some(context))
    }

    #[must_use]
    pub fn root(&self, index: usize) -> Option<RootId> {
        self.roots.get(index).map(|_| RootId(index))
    }

    #[must_use]
    pub fn node_count(&self) -> usize { self.nodes.len() }

    /// Samples without allocation, resource lookup, hashing or synchronization.
    /// Conditional branches and zero-valued multiply/divide inputs stay lazy.
    pub fn sample(&self, context: BlockContext, workspace: &mut EvalWorkspace) -> Result<f32, BuildError> {
        self.sample_root(RootId(0), context, workspace)
    }

    /// Samples a root using the same request state as every other consumer.
    pub fn sample_root(&self, root: RootId, context: BlockContext, workspace: &mut EvalWorkspace) -> Result<f32, BuildError> {
        let root = *self.roots.get(root.0).ok_or_else(|| BuildError::new("sample", "invalid density root"))?;
        if workspace.memo.len() < self.nodes.len() || workspace.volume_capacity < self.scalar_capacity
            || workspace.volume_scratch.len() < self.scratch_frames.saturating_mul(self.scalar_capacity) {
            return Err(BuildError::new("sample", "workspace requires preparation for this density program"));
        }
        workspace.epoch = workspace.epoch.wrapping_add(1);
        if workspace.epoch == 0 {
            workspace.memo.fill(Memo::default());
            workspace.epoch = 1;
        }
        self.evaluate(root, context, workspace)
    }

    fn evaluate(&self, id: NodeId, context: BlockContext, workspace: &mut EvalWorkspace) -> Result<f32, BuildError> {
        let mut state=EvalState {memo:&mut workspace.memo,epoch:workspace.epoch,caches:&mut workspace.cache_slots,
            request_context:workspace.request_context.as_deref()};
        self.evaluate_scalar(id,context,&mut state,&mut workspace.volume_scratch,workspace.volume_capacity)
    }

    fn evaluate_scalar(&self,id:NodeId,context:BlockContext,state:&mut EvalState<'_>,scratch:&mut[f32],capacity:usize)->Result<f32,BuildError> {
        let memo = state.memo[id.0];
        if !self.effects[id.0] && memo.epoch == state.epoch && memo.context == context {
            return Ok(memo.value);
        }
        let value = match self.nodes[id.0] {
            Node::Constant(value) => value,
            Node::Context(input) => state.request_context()?.sample(input,context)?,
            Node::BlendDensity {input} => {
                let density=self.evaluate_scalar(input,context,state,scratch,capacity)?;
                state.request_context()?.blend_density(context,density)?
            }
            Node::Distance {point,metric} => metric.sample(point,context),
            Node::EndIslands => self.islands.as_ref().expect("compiled island sampler").sample(context.x,context.z),
            Node::Unary { operation, input } => {
                let value = self.evaluate_scalar(input,context,state,scratch,capacity)?;
                match operation {
                    Unary::Abs => value.abs(),
                    Unary::Square => value * value,
                    Unary::Cube => (value * value) * value,
                    Unary::HalfNegative => if value > 0.0 { value } else { value * 0.5 },
                    Unary::QuarterNegative => if value > 0.0 { value } else { value * 0.25 },
                    Unary::Reciprocal => 1.0 / value,
                    Unary::Negate => -value,
                    Unary::Squeeze => {
                        let value = java_clamp(value, -1.0, 1.0);
                        value / 2.0 - ((value * value) * value) / 24.0
                    }
                    Unary::Sign => if value == 0.0 || value.is_nan() { value } else { value.signum() },
                }
            }
            Node::Binary { operation, left, right } => {
                let left = self.evaluate_scalar(left,context,state,scratch,capacity)?;
                if left == 0.0 && matches!(operation, Binary::Multiply | Binary::Divide) {
                    0.0
                } else {
                    let right = self.evaluate_scalar(right,context,state,scratch,capacity)?;
                    match operation {
                        Binary::Add => left + right,
                        Binary::Subtract => left - right,
                        Binary::Multiply => left * right,
                        Binary::Divide => left / right,
                        Binary::Min => java_min(left, right),
                        Binary::Max => java_max(left, right),
                    }
                }
            }
            Node::Clamp { input, min, max } => java_clamp(self.evaluate_scalar(input,context,state,scratch,capacity)?,min,max),
            Node::Lerp { alpha, first, second } => {
                let alpha = self.evaluate_scalar(alpha,context,state,scratch,capacity)?;
                if alpha == 0.0 {
                    self.evaluate_scalar(first,context,state,scratch,capacity)?
                } else if alpha == 1.0 {
                    self.evaluate_scalar(second,context,state,scratch,capacity)?
                } else {
                    let first = self.evaluate_scalar(first,context,state,scratch,capacity)?;
                    let second = self.evaluate_scalar(second,context,state,scratch,capacity)?;
                    first + alpha * (second - first)
                }
            }
            Node::RangeChoice { input, min, max, inside, outside } => {
                let input = self.evaluate_scalar(input,context,state,scratch,capacity)?;
                self.evaluate_scalar(if input >= min && input < max {inside} else {outside},context,state,scratch,capacity)?
            }
            Node::Gradient { axis, from, low, high, value, factor } => {
                let delta = axis.get(context).clamp(low, high).wrapping_sub(from);
                value + delta as f32 * factor
            }
            Node::Slice { axis, coordinate, input } => {
                self.evaluate_scalar(input,axis.replace(context,coordinate),state,scratch,capacity)?
            }
            Node::Noise { sampler, xz_scale, y_scale, shifts } => {
                let mut position = [f64::from(context.x) * xz_scale,
                    f64::from(context.y) * y_scale, f64::from(context.z) * xz_scale];
                for (coordinate, shift) in position.iter_mut().zip(shifts) {
                    if let Some(shift) = shift { *coordinate += f64::from(self.evaluate_scalar(shift,context,state,scratch,capacity)?); }
                }
                self.noises[sampler].sample(position[0], position[1], position[2])
            }
            Node::Shift { sampler, mapping } => {
                let x = f64::from(context.x) * 0.25;
                let y = f64::from(context.y) * 0.25;
                let z = f64::from(context.z) * 0.25;
                let position = match mapping {
                    Shift::ThreeDimensional => [x,y,z],
                    Shift::Horizontal => [x,0.0,z],
                    Shift::RotatedHorizontal => [z,x,0.0],
                };
                self.noises[sampler].sample(position[0],position[1],position[2]) * 4.0
            }
            Node::Interpolated { input,xz,y,.. } => self.interpolate_scalar(input,xz,y,context,state,scratch,capacity)?,
            Node::Cache { input,slot } => {
                if let Some(value) = state.caches[slot].sample(context) { value }
                else {
                    let value = self.evaluate_scalar(input,context,state,scratch,capacity)?;
                    state.caches[slot].point = Some((context,value));
                    value
                }
            }
            Node::FindTopSurface {density,upper,lower,step} => {
                let upper=self.evaluate_scalar(upper,context,state,scratch,capacity)?;
                self.find_surface(density,lower,step,upper,context,state,scratch,capacity)? as f32
            }
            Node::Selection {table} => {
                let selection=&self.tables.selections[table];
                let value=self.evaluate_scalar(selection.input,context,state,scratch,capacity)?;
                self.evaluate_scalar(selection.functions[selection.index(value)],context,state,scratch,capacity)?
            }
            Node::Spline {table} => self.spline_scalar(table,context,state,scratch,capacity)?,
        };
        if !self.effects[id.0] { state.memo[id.0] = Memo { epoch: state.epoch, context, value }; }
        Ok(value)
    }
}

fn java_min(a: f32, b: f32) -> f32 {
    if a.is_nan() { return a; }
    if b.is_nan() { return b; }
    if a == 0.0 && b == 0.0 { return f32::from_bits(a.to_bits() | b.to_bits()); }
    if a <= b { a } else { b }
}

fn java_max(a: f32, b: f32) -> f32 {
    if a.is_nan() { return a; }
    if b.is_nan() { return b; }
    if a == 0.0 && b == 0.0 { return f32::from_bits(a.to_bits() & b.to_bits()); }
    if a >= b { a } else { b }
}

fn java_clamp(value: f32, min: f32, max: f32) -> f32 {
    if value < min { min } else if value > max { max } else { value }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn value(document: Value) -> f32 {
        let program = Program::parse(&document, &|_| None).unwrap();
        program.sample(BlockContext::default(), &mut EvalWorkspace::new(&program)).unwrap()
    }

    #[test]
    fn each_arithmetic_node_rounds_before_its_parent() {
        let document = json!({"type":"minecraft:sub", "left":{
            "type":"minecraft:add", "left":{"type":"mul", "left":4096, "right":4096}, "right":1},
            "right":{"type":"mul", "left":4096, "right":4096}});
        assert_eq!(value(document).to_bits(), 0.0_f32.to_bits());
        assert_eq!((16_777_216_f64 + 1.0) - 16_777_216.0, 1.0);
    }

    #[test]
    fn multiply_and_divide_zero_skip_the_other_branch_and_return_positive_zero() {
        for operation in ["mul", "div"] {
            let program = Program::parse(&json!({"type":operation, "left":-0.0,
                "right":{"type":"reciprocal", "input":0}}), &|_| None).unwrap();
            let mut workspace = EvalWorkspace::new(&program);
            assert_eq!(program.sample(BlockContext::default(), &mut workspace).unwrap().to_bits(), 0);
            let right=program.nodes.iter().find_map(|node|if let Node::Binary {right,..}=node {Some(*right)} else {None}).unwrap();
            assert_eq!(workspace.memo[right.0].epoch, 0);
        }
    }

    #[test]
    fn selection_boundaries_and_lerp_endpoints_are_lazy() {
        let program = Program::parse(&json!({"type":"lerp", "alpha":1,
            "first":{"type":"reciprocal", "input":0}, "second":13.25}), &|_| None).unwrap();
        let mut workspace = EvalWorkspace::new(&program);
        assert_eq!(program.sample(BlockContext::default(), &mut workspace).unwrap(), 13.25);
        let first=program.nodes.iter().find_map(|node|if let Node::Lerp {first,..}=node {Some(*first)} else {None}).unwrap();
        assert_eq!(workspace.memo[first.0].epoch, 0);
        assert_eq!(value(json!({"type":"range_choice", "input":0.375, "min_inclusive":0.125,
            "max_exclusive":0.375, "when_in_range":-17, "when_out_of_range":23})), 23.0);
    }

    #[test]
    fn shared_references_sample_again_when_slice_changes_coordinates() {
        let lookup = |id: &str| (id == "minecraft:ramp").then(|| json!({"type":"gradient", "axis":"x",
            "from_coordinate":-8, "to_coordinate":8, "from_value":-2, "to_value":2}));
        let program = Program::parse(&json!({"type":"add", "left":"ramp",
            "right":{"type":"slice", "axis":"x", "coordinate":6, "input":"ramp"}}), &lookup).unwrap();
        let mut workspace = EvalWorkspace::new(&program);
        assert_eq!(program.nodes.iter().filter(|node|matches!(node,Node::Gradient {..})).count(),1);
        assert_eq!(program.sample(BlockContext { x:-3, y:0, z:0 }, &mut workspace).unwrap(), 0.75);
        assert_eq!(program.sample(BlockContext { x:1, y:0, z:0 }, &mut workspace).unwrap(), 1.75);
    }

    #[test]
    fn reversed_gradient_rounds_its_factor_before_sampling() {
        let program = Program::parse(&json!({"type":"gradient", "axis":"z",
            "from_coordinate":13, "to_coordinate":-7,
            "from_value":0.3125, "to_value":-0.84375}), &|_| None).unwrap();
        let result = program.sample(BlockContext { x:0, y:0, z:9 }, &mut EvalWorkspace::new(&program)).unwrap();
        // Independent binary32 arithmetic: rounded slope 0.05781250074505806,
        // integer delta -4, then a rounded product followed by a rounded sum.
        assert_eq!(result.to_bits(), 0x3da6_6666);
    }

    #[test]
    fn signed_zero_and_nan_follow_float_min_max_contract() {
        assert_eq!(java_min(0.0, -0.0).to_bits(), 0x8000_0000);
        assert_eq!(java_max(-0.0, 0.0).to_bits(), 0);
        assert!(java_min(f32::NAN, 3.0).is_nan());
        assert!(java_max(3.0, f32::NAN).is_nan());
        assert_eq!(value(json!({"type":"sign", "input":-0.0})).to_bits(), 0x8000_0000);
    }
}
