//! The 26.3 density-function expression tree.
//!
//! Nodes are hash-consed into an arena, so two nodes are structurally equal
//! exactly when their ids are equal. That is not a convenience: the compiler
//! deduplicates caches and spline coordinates by structural equality, and which
//! results a cache serves depends on which expressions it considers the same.
//! Floats inside nodes are compared by bit pattern, like the reference's record
//! equality.

use std::collections::HashMap;

use serde_json::Value;

use super::interval::{jmax, Interval};
use super::noise::{blended_range, NoiseParams, Normalization, NormalNoise};

pub type NodeId = u32;
pub type NameId = u32;

/// Bit set of the axes a function's value varies along.
pub type Axes = u8;
pub const AXIS_X: Axes = 1;
pub const AXIS_Y: Axes = 2;
pub const AXIS_Z: Axes = 4;
pub const ALL_AXES: Axes = 7;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Axis {
    X,
    Y,
    Z,
}

impl Axis {
    pub fn mask(self) -> Axes {
        match self {
            Self::X => AXIS_X,
            Self::Y => AXIS_Y,
            Self::Z => AXIS_Z,
        }
    }

    pub fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tiling {
    ClampToEdge,
    Repeat,
    MirroredRepeat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Metric {
    Euclidean,
    EuclideanSquared,
    Manhattan,
    Chebyshev,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Unary {
    Abs,
    Square,
    Cube,
    Sqrt,
    HalfNegative,
    QuarterNegative,
    Reciprocal,
    Negate,
    Squeeze,
    Log,
    Sign,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Binary {
    Add,
    Sub,
    Mul,
    Div,
    Min,
    Max,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RoundKind {
    Floor,
    Round,
    Ceil,
    Truncate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShiftKind {
    /// Three-axis offset noise at a quarter scale.
    Shift,
    /// Offset noise over X and Z only, sampled at `(x, 0, z)`.
    A,
    /// The A noise with its axes swapped: sampled at `(z, x, 0)`.
    B,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Context {
    BlendAlpha,
    BlendOffset,
    Beardifier,
}

/// A cubic spline over density-function coordinates.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Spline {
    Const(u32),
    Multi { coordinate: NodeId, locations: Vec<u32>, values: Vec<Spline>, derivatives: Vec<u32> },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Node {
    Const(u32),
    /// A registry reference; inlined by the compiler.
    Ref(NameId),
    Context(Context),
    Noise { noise: NameId, xz_scale: u64, y_scale: u64, shift: [NodeId; 3] },
    Shift { kind: ShiftKind, noise: NameId },
    EndIslands,
    DistanceToPoint { point: [i32; 3], metric: Metric },
    Gradient { axis: Axis, tiling: Tiling, from: i32, to: i32, from_value: u32, to_value: u32 },
    Unary(Unary, NodeId),
    Round { kind: RoundKind, input: NodeId, multiple: NodeId },
    Binary(Binary, NodeId, NodeId),
    Pow(NodeId, NodeId),
    Spline(Spline),
    Lerp(NodeId, NodeId, NodeId),
    Clamp { input: NodeId, min: u32, max: u32 },
    RangeChoice { input: NodeId, min_inclusive: u32, max_exclusive: u32, in_range: NodeId, out_of_range: NodeId },
    IntervalSelect { input: NodeId, thresholds: Vec<u32>, functions: Vec<NodeId> },
    Cache(NodeId),
    BlendDensity(NodeId),
    Interpolated { input: NodeId, cell_xz: i32, cell_y: i32 },
    Slice { axis: Axis, coordinate: i32, input: NodeId },
    FindTopSurface { density: NodeId, upper_bound: NodeId, lower_bound: i32, cell_height: i32 },
    OldBlendedNoise { xz_scale: u64, y_scale: u64, xz_factor: u64, y_factor: u64, smear: u64 },
    /// A cache the compiler has deduplicated; the id names its cell.
    Prepared(u32),
}

/// What a deduplicated cache remembers about its input.
#[derive(Clone, Copy, Debug)]
pub struct PreparedInfo {
    pub range: Interval,
    pub axes: Axes,
    /// The cached sampler compiled from the rewritten input.
    pub sampler: u32,
}

/// Access to the load-time resources a tree refers to.
pub trait Resources {
    fn density_function(&self, name: &str) -> Option<&Value>;
    fn noise(&self, name: &str) -> Option<&Value>;
}

#[derive(Debug)]
pub enum TreeError {
    Missing(String),
    Invalid(String),
}

impl std::fmt::Display for TreeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(m) => write!(f, "missing resource {m}"),
            Self::Invalid(m) => write!(f, "invalid worldgen data: {m}"),
        }
    }
}

impl std::error::Error for TreeError {}

fn invalid<T>(msg: impl Into<String>) -> Result<T, TreeError> {
    Err(TreeError::Invalid(msg.into()))
}

/// Hash-consed node arena plus the registries it was parsed against.
#[derive(Default)]
pub struct Tree {
    nodes: Vec<Node>,
    index: HashMap<Node, NodeId>,
    names: Vec<String>,
    name_index: HashMap<String, NameId>,
    /// Registry name -> the function it refers to.
    registry: HashMap<NameId, NodeId>,
    noises: HashMap<NameId, NormalNoise>,
    pub prepared: Vec<PreparedInfo>,
    range_memo: HashMap<NodeId, Interval>,
    axes_memo: HashMap<NodeId, Axes>,
}

pub fn normalize_name(name: &str) -> String {
    if name.contains(':') { name.to_owned() } else { format!("minecraft:{name}") }
}

fn short_type(t: &str) -> &str {
    t.strip_prefix("minecraft:").unwrap_or(t)
}

pub fn f32_bits(v: f32) -> u32 {
    v.to_bits()
}

pub fn f64_bits(v: f64) -> u64 {
    v.to_bits()
}

impl Tree {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn intern(&mut self, node: Node) -> NodeId {
        if let Some(&id) = self.index.get(&node) {
            return id;
        }
        let id = self.nodes.len() as NodeId;
        self.nodes.push(node.clone());
        self.index.insert(node, id);
        id
    }

    pub fn constant(&mut self, v: f32) -> NodeId {
        self.intern(Node::Const(f32_bits(v)))
    }

    pub fn name(&self, id: NameId) -> &str {
        &self.names[id as usize]
    }

    pub fn name_id(&mut self, name: &str) -> NameId {
        let n = normalize_name(name);
        if let Some(&id) = self.name_index.get(&n) {
            return id;
        }
        let id = self.names.len() as NameId;
        self.names.push(n.clone());
        self.name_index.insert(n, id);
        id
    }

    /// The function a registry reference names.
    pub fn registry_value(&self, name: NameId) -> NodeId {
        self.registry[&name]
    }

    pub fn noise_def(&self, name: NameId) -> &NormalNoise {
        &self.noises[&name]
    }

    // ---- parsing ----

    /// Parses a density function: a number, a registry name, or an object.
    pub fn parse(&mut self, value: &Value, res: &dyn Resources) -> Result<NodeId, TreeError> {
        match value {
            Value::Number(n) => Ok(self.constant(n.as_f64().ok_or_else(|| TreeError::Invalid("number".into()))? as f32)),
            Value::String(s) => self.reference(s, res),
            Value::Object(_) => self.parse_object(value, res),
            other => invalid(format!("not a density function: {other}")),
        }
    }

    /// A registry reference, loading the referenced definition on first use.
    pub fn reference(&mut self, name: &str, res: &dyn Resources) -> Result<NodeId, TreeError> {
        let id = self.name_id(name);
        if !self.registry.contains_key(&id) {
            let key = self.name(id).to_owned();
            let doc = res.density_function(&key).ok_or_else(|| TreeError::Missing(format!("density_function {key}")))?;
            let doc = doc.clone();
            let root = self.parse(&doc, res)?;
            self.registry.insert(id, root);
        }
        Ok(self.intern(Node::Ref(id)))
    }

    fn noise_ref(&mut self, value: &Value, res: &dyn Resources) -> Result<NameId, TreeError> {
        let Value::String(name) = value else { return invalid("noise must be a registry name") };
        let id = self.name_id(name);
        if !self.noises.contains_key(&id) {
            let key = self.name(id).to_owned();
            let doc = res.noise(&key).ok_or_else(|| TreeError::Missing(format!("noise {key}")))?;
            self.noises.insert(id, NormalNoise::new(parse_noise_params(doc)?));
        }
        Ok(id)
    }

    fn field<'v>(obj: &'v Value, key: &str) -> Result<&'v Value, TreeError> {
        obj.get(key).ok_or_else(|| TreeError::Invalid(format!("missing field {key}")))
    }

    fn f32_field(obj: &Value, key: &str) -> Result<f32, TreeError> {
        Self::field(obj, key)?.as_f64().map(|v| v as f32).ok_or_else(|| TreeError::Invalid(format!("{key} is not a number")))
    }

    fn f64_field(obj: &Value, key: &str) -> Result<f64, TreeError> {
        Self::field(obj, key)?.as_f64().ok_or_else(|| TreeError::Invalid(format!("{key} is not a number")))
    }

    fn i32_field(obj: &Value, key: &str) -> Result<i32, TreeError> {
        Self::field(obj, key)?.as_i64().map(|v| v as i32).ok_or_else(|| TreeError::Invalid(format!("{key} is not an integer")))
    }

    fn child(&mut self, obj: &Value, key: &str, res: &dyn Resources) -> Result<NodeId, TreeError> {
        let v = Self::field(obj, key)?.clone();
        self.parse(&v, res)
    }

    fn axis(obj: &Value) -> Result<Axis, TreeError> {
        match Self::field(obj, "axis")?.as_str() {
            Some("x") => Ok(Axis::X),
            Some("y") => Ok(Axis::Y),
            Some("z") => Ok(Axis::Z),
            other => invalid(format!("bad axis {other:?}")),
        }
    }

    fn parse_object(&mut self, obj: &Value, res: &dyn Resources) -> Result<NodeId, TreeError> {
        let ty = Self::field(obj, "type")?.as_str().ok_or_else(|| TreeError::Invalid("type".into()))?;
        let ty = short_type(ty).to_owned();
        let node = match ty.as_str() {
            "constant" => Node::Const(f32_bits(Self::f32_field(obj, "value")?)),
            "blend_alpha" => Node::Context(Context::BlendAlpha),
            "blend_offset" => Node::Context(Context::BlendOffset),
            "beardifier" => Node::Context(Context::Beardifier),
            "noise" => {
                let noise = self.noise_ref(Self::field(obj, "noise")?, res)?;
                let zero = self.constant(0.0);
                let mut shift = [zero; 3];
                for (slot, key) in ["shift_x", "shift_y", "shift_z"].into_iter().enumerate() {
                    if obj.get(key).is_some() {
                        shift[slot] = self.child(obj, key, res)?;
                    }
                }
                Node::Noise {
                    noise,
                    xz_scale: f64_bits(Self::f64_field(obj, "xz_scale")?),
                    y_scale: f64_bits(Self::f64_field(obj, "y_scale")?),
                    shift,
                }
            }
            "shift" | "shift_a" | "shift_b" => {
                let noise = self.noise_ref(Self::field(obj, "noise")?, res)?;
                let kind = match ty.as_str() {
                    "shift" => ShiftKind::Shift,
                    "shift_a" => ShiftKind::A,
                    _ => ShiftKind::B,
                };
                Node::Shift { kind, noise }
            }
            "end_outer_islands" => Node::EndIslands,
            "distance_to_point" => {
                let p = Self::field(obj, "point")?.as_array().ok_or_else(|| TreeError::Invalid("point".into()))?;
                if p.len() != 3 {
                    return invalid("point must have three components");
                }
                let comp = |i: usize| p[i].as_i64().map(|v| v as i32).ok_or_else(|| TreeError::Invalid("point component".into()));
                let metric = match Self::field(obj, "metric")?.as_str() {
                    Some("euclidean") => Metric::Euclidean,
                    Some("euclidean_squared") => Metric::EuclideanSquared,
                    Some("manhattan") => Metric::Manhattan,
                    Some("chebyshev") => Metric::Chebyshev,
                    other => return invalid(format!("metric {other:?}")),
                };
                Node::DistanceToPoint { point: [comp(0)?, comp(1)?, comp(2)?], metric }
            }
            "gradient" => {
                let tiling = match obj.get("tiling").and_then(Value::as_str) {
                    None | Some("clamp_to_edge") => Tiling::ClampToEdge,
                    Some("repeat") => Tiling::Repeat,
                    Some("mirrored_repeat") => Tiling::MirroredRepeat,
                    other => return invalid(format!("tiling {other:?}")),
                };
                let from = Self::i32_field(obj, "from_coordinate")?;
                let to = Self::i32_field(obj, "to_coordinate")?;
                if from == to {
                    return invalid("from_coordinate cannot equal to_coordinate");
                }
                Node::Gradient {
                    axis: Self::axis(obj)?,
                    tiling,
                    from,
                    to,
                    from_value: f32_bits(Self::f32_field(obj, "from_value")?),
                    to_value: f32_bits(Self::f32_field(obj, "to_value")?),
                }
            }
            "abs" | "square" | "cube" | "sqrt" | "half_negative" | "quarter_negative" | "reciprocal" | "negate"
            | "squeeze" | "log" | "sign" => {
                let kind = match ty.as_str() {
                    "abs" => Unary::Abs,
                    "square" => Unary::Square,
                    "cube" => Unary::Cube,
                    "sqrt" => Unary::Sqrt,
                    "half_negative" => Unary::HalfNegative,
                    "quarter_negative" => Unary::QuarterNegative,
                    "reciprocal" => Unary::Reciprocal,
                    "negate" => Unary::Negate,
                    "squeeze" => Unary::Squeeze,
                    "log" => Unary::Log,
                    _ => Unary::Sign,
                };
                Node::Unary(kind, self.child(obj, "input", res)?)
            }
            "floor" | "round" | "ceil" | "truncate" => {
                let kind = match ty.as_str() {
                    "floor" => RoundKind::Floor,
                    "round" => RoundKind::Round,
                    "ceil" => RoundKind::Ceil,
                    _ => RoundKind::Truncate,
                };
                let input = self.child(obj, "input", res)?;
                let multiple = if obj.get("multiple").is_some() { self.child(obj, "multiple", res)? } else { self.constant(1.0) };
                Node::Round { kind, input, multiple }
            }
            "add" | "sub" | "mul" | "div" | "min" | "max" => {
                let kind = match ty.as_str() {
                    "add" => Binary::Add,
                    "sub" => Binary::Sub,
                    "mul" => Binary::Mul,
                    "div" => Binary::Div,
                    "min" => Binary::Min,
                    _ => Binary::Max,
                };
                Node::Binary(kind, self.child(obj, "left", res)?, self.child(obj, "right", res)?)
            }
            "pow" => Node::Pow(self.child(obj, "base", res)?, self.child(obj, "exponent", res)?),
            "spline" => {
                let spline = Self::field(obj, "spline")?.clone();
                Node::Spline(self.parse_spline(&spline, res)?)
            }
            "lerp" => Node::Lerp(
                self.child(obj, "alpha", res)?,
                self.child(obj, "first", res)?,
                self.child(obj, "second", res)?,
            ),
            "clamp" => {
                let (min, max) = (Self::f32_field(obj, "min")?, Self::f32_field(obj, "max")?);
                if max < min {
                    return invalid("clamp max < min");
                }
                Node::Clamp { input: self.child(obj, "input", res)?, min: f32_bits(min), max: f32_bits(max) }
            }
            "range_choice" => Node::RangeChoice {
                input: self.child(obj, "input", res)?,
                min_inclusive: f32_bits(Self::f32_field(obj, "min_inclusive")?),
                max_exclusive: f32_bits(Self::f32_field(obj, "max_exclusive")?),
                in_range: self.child(obj, "when_in_range", res)?,
                out_of_range: self.child(obj, "when_out_of_range", res)?,
            },
            "interval_select" => {
                let thresholds: Vec<u32> = Self::field(obj, "thresholds")?
                    .as_array()
                    .ok_or_else(|| TreeError::Invalid("thresholds".into()))?
                    .iter()
                    .map(|v| v.as_f64().map(|f| f32_bits(f as f32)).ok_or_else(|| TreeError::Invalid("threshold".into())))
                    .collect::<Result<_, _>>()?;
                let fns = Self::field(obj, "functions")?.as_array().ok_or_else(|| TreeError::Invalid("functions".into()))?.clone();
                if thresholds.len() + 1 != fns.len() || fns.len() < 2 {
                    return invalid("interval_select needs one more function than thresholds");
                }
                let functions = fns.iter().map(|f| self.parse(f, res)).collect::<Result<Vec<_>, _>>()?;
                Node::IntervalSelect { input: self.child(obj, "input", res)?, thresholds, functions }
            }
            "cache" => Node::Cache(self.child(obj, "input", res)?),
            "blend_density" => Node::BlendDensity(self.child(obj, "input", res)?),
            "interpolated" => Node::Interpolated {
                input: self.child(obj, "input", res)?,
                cell_xz: Self::i32_field(obj, "cell_size_xz")?,
                cell_y: Self::i32_field(obj, "cell_size_y")?,
            },
            "slice" => Node::Slice {
                axis: Self::axis(obj)?,
                coordinate: Self::i32_field(obj, "coordinate")?,
                input: self.child(obj, "input", res)?,
            },
            "find_top_surface" => Node::FindTopSurface {
                density: self.child(obj, "density", res)?,
                upper_bound: self.child(obj, "upper_bound", res)?,
                lower_bound: Self::i32_field(obj, "lower_bound")?,
                cell_height: Self::i32_field(obj, "cell_height")?,
            },
            "old_blended_noise" => Node::OldBlendedNoise {
                xz_scale: f64_bits(Self::f64_field(obj, "xz_scale")?),
                y_scale: f64_bits(Self::f64_field(obj, "y_scale")?),
                xz_factor: f64_bits(Self::f64_field(obj, "xz_factor")?),
                y_factor: f64_bits(Self::f64_field(obj, "y_factor")?),
                smear: f64_bits(Self::f64_field(obj, "smear_scale_multiplier")?),
            },
            other => return invalid(format!("unknown density function type {other}")),
        };
        Ok(self.intern(node))
    }

    fn parse_spline(&mut self, v: &Value, res: &dyn Resources) -> Result<Spline, TreeError> {
        if let Some(n) = v.as_f64() {
            return Ok(Spline::Const(f32_bits(n as f32)));
        }
        let coord = Self::field(v, "coordinate")?.clone();
        let coordinate = self.parse(&coord, res)?;
        let points = Self::field(v, "points")?.as_array().ok_or_else(|| TreeError::Invalid("points".into()))?;
        if points.is_empty() {
            return invalid("spline needs at least one point");
        }
        let mut locations = Vec::new();
        let mut values = Vec::new();
        let mut derivatives = Vec::new();
        for p in points {
            locations.push(f32_bits(Self::f32_field(p, "location")?));
            derivatives.push(f32_bits(Self::f32_field(p, "derivative")?));
            let value = Self::field(p, "value")?.clone();
            values.push(self.parse_spline(&value, res)?);
        }
        Ok(Spline::Multi { coordinate, locations, values, derivatives })
    }

    // ---- structure queries ----

    /// Children in rewrite order (the order the reference visits them).
    pub fn children(&self, id: NodeId) -> Vec<NodeId> {
        match self.node(id) {
            Node::Noise { shift, .. } => shift.to_vec(),
            Node::Unary(_, i) | Node::Cache(i) | Node::BlendDensity(i) => vec![*i],
            Node::Interpolated { input, .. } | Node::Slice { input, .. } | Node::Clamp { input, .. } => vec![*input],
            Node::Round { input, multiple, .. } => vec![*input, *multiple],
            Node::Binary(_, l, r) | Node::Pow(l, r) => vec![*l, *r],
            Node::Lerp(a, f, s) => vec![*a, *f, *s],
            Node::RangeChoice { input, in_range, out_of_range, .. } => vec![*input, *in_range, *out_of_range],
            Node::IntervalSelect { input, functions, .. } => {
                let mut v = vec![*input];
                v.extend(functions.iter().copied());
                v
            }
            Node::FindTopSurface { density, upper_bound, .. } => vec![*density, *upper_bound],
            Node::Spline(s) => {
                let mut v = Vec::new();
                spline_coordinates(s, &mut v);
                v
            }
            _ => Vec::new(),
        }
    }

    /// The conservative value range of a function.
    pub fn range(&mut self, id: NodeId) -> Interval {
        if let Some(r) = self.range_memo.get(&id) {
            return *r;
        }
        let r = self.compute_range(id);
        self.range_memo.insert(id, r);
        r
    }

    fn compute_range(&mut self, id: NodeId) -> Interval {
        let node = self.node(id).clone();
        match node {
            Node::Const(b) => Interval::exact(f32::from_bits(b)),
            Node::Ref(n) => {
                let v = self.registry_value(n);
                self.range(v)
            }
            Node::Context(Context::BlendAlpha) => Interval::of(0.0, 1.0),
            Node::Context(Context::BlendOffset) | Node::Context(Context::Beardifier) => Interval::INFINITE,
            Node::Noise { noise, .. } => self.noises[&noise].range(),
            Node::Shift { noise, .. } => Interval::mul(self.noises[&noise].range(), Interval::exact(4.0)),
            Node::EndIslands => Interval::of(-0.84375, 0.5625),
            Node::DistanceToPoint { .. } => Interval::of(0.0, f32::INFINITY),
            Node::Gradient { from_value, to_value, .. } => {
                Interval::encapsulating2(f32::from_bits(from_value), f32::from_bits(to_value))
            }
            Node::Unary(kind, input) => {
                let i = self.range(input);
                unary_range(kind, i)
            }
            Node::Round { kind, input, multiple } => {
                let m = self.range(multiple);
                let i = self.range(input);
                Interval::mul(Interval::map_monotonic(Interval::div(i, m), |v| round_to_integer(v, kind)), m)
            }
            Node::Binary(kind, l, r) => {
                let (l, r) = (self.range(l), self.range(r));
                match kind {
                    Binary::Add => Interval::add(l, r),
                    Binary::Sub => Interval::sub(l, r),
                    Binary::Mul => Interval::mul(l, r),
                    Binary::Div => Interval::div(l, r),
                    Binary::Min => Interval::min_of(l, r),
                    Binary::Max => Interval::max_of(l, r),
                }
            }
            Node::Pow(b, e) => {
                let (b, e) = (self.range(b), self.range(e));
                Interval::pow(b, e)
            }
            Node::Spline(s) => self.spline_range(&s),
            Node::Lerp(a, f, s) => {
                let (a, f, s) = (self.range(a), self.range(f), self.range(s));
                Interval::lerp(a, f, s)
            }
            Node::Clamp { input, min, max } => {
                let i = self.range(input);
                Interval::clamp(i, f32::from_bits(min), f32::from_bits(max))
            }
            Node::RangeChoice { in_range, out_of_range, .. } => {
                let (a, b) = (self.range(in_range), self.range(out_of_range));
                Interval::encapsulating_all(&[a, b])
            }
            Node::IntervalSelect { functions, .. } => {
                let ranges: Vec<Interval> = functions.iter().map(|f| self.range(*f)).collect();
                Interval::encapsulating_all(&ranges)
            }
            Node::Cache(i) | Node::BlendDensity(i) => self.range(i),
            Node::Interpolated { input, .. } | Node::Slice { input, .. } => self.range(input),
            Node::FindTopSurface { upper_bound, lower_bound, .. } => {
                let upper = self.range(upper_bound);
                Interval::of(lower_bound as f32, jmax(lower_bound as f32, upper.max()))
            }
            Node::OldBlendedNoise { y_scale, smear, .. } => blended_range(f64::from_bits(y_scale), f64::from_bits(smear)),
            Node::Prepared(p) => self.prepared[p as usize].range,
        }
    }

    fn spline_range(&mut self, s: &Spline) -> Interval {
        match s {
            Spline::Const(b) => Interval::exact(f32::from_bits(*b)),
            Spline::Multi { coordinate, locations, values, derivatives } => {
                let locations: Vec<f32> = locations.iter().map(|b| f32::from_bits(*b)).collect();
                let derivatives: Vec<f32> = derivatives.iter().map(|b| f32::from_bits(*b)).collect();
                let last = locations.len() - 1;
                let mut min_value = f32::INFINITY;
                let mut max_value = f32::NEG_INFINITY;
                let input = self.range(*coordinate);
                if input.is_nai() {
                    return input;
                }
                let extend = |input: f32, value: f32, index: usize| {
                    let d = derivatives[index];
                    if d == 0.0 { value } else { value + d * (input - locations[index]) }
                };
                let value_ranges: Vec<Interval> = values.iter().map(|v| self.spline_range(v)).collect();
                if input.min() < locations[0] {
                    let first = value_ranges[0];
                    let e1 = extend(input.min(), first.min(), 0);
                    let e2 = extend(input.min(), first.max(), 0);
                    min_value = super::interval::jmin(min_value, super::interval::jmin(e1, e2));
                    max_value = jmax(max_value, jmax(e1, e2));
                }
                if input.max() > locations[last] {
                    let lastr = value_ranges[last];
                    let e1 = extend(input.max(), lastr.min(), last);
                    let e2 = extend(input.max(), lastr.max(), last);
                    min_value = super::interval::jmin(min_value, super::interval::jmin(e1, e2));
                    max_value = jmax(max_value, jmax(e1, e2));
                }
                for r in &value_ranges {
                    min_value = super::interval::jmin(min_value, r.min());
                    max_value = jmax(max_value, r.max());
                }
                for i in 0..last {
                    let x_diff = locations[i + 1] - locations[i];
                    let (r1, r2) = (value_ranges[i], value_ranges[i + 1]);
                    let (min1, max1, min2, max2) = (r1.min(), r1.max(), r2.min(), r2.max());
                    let (d1, d2) = (derivatives[i], derivatives[i + 1]);
                    if d1 != 0.0 || d2 != 0.0 {
                        let p1 = d1 * x_diff;
                        let p2 = d2 * x_diff;
                        let min_lerp1 = super::interval::jmin(min1, min2);
                        let max_lerp1 = jmax(max1, max2);
                        let min_a = p1 - max2 + min1;
                        let max_a = p1 - min2 + max1;
                        let min_b = -p2 + min2 - max1;
                        let max_b = -p2 + max2 - min1;
                        let min_lerp2 = super::interval::jmin(min_a, min_b);
                        let max_lerp2 = jmax(max_a, max_b);
                        min_value = super::interval::jmin(min_value, min_lerp1 + 0.25 * min_lerp2);
                        max_value = jmax(max_value, max_lerp1 + 0.25 * max_lerp2);
                    }
                }
                Interval::of(min_value, max_value)
            }
        }
    }

    /// The axes a function's value depends on.
    pub fn axes(&mut self, id: NodeId) -> Axes {
        if let Some(a) = self.axes_memo.get(&id) {
            return *a;
        }
        let a = self.compute_axes(id);
        self.axes_memo.insert(id, a);
        a
    }

    fn compute_axes(&mut self, id: NodeId) -> Axes {
        let node = self.node(id).clone();
        match node {
            Node::Const(_) => 0,
            Node::Ref(n) => {
                let v = self.registry_value(n);
                self.axes(v)
            }
            Node::Context(Context::BlendAlpha | Context::BlendOffset) => 5,
            Node::Context(Context::Beardifier) => 7,
            Node::Noise { xz_scale, y_scale, shift, .. } => {
                let mut axes = ALL_AXES;
                if f64::from_bits(y_scale) == 0.0 {
                    axes &= !AXIS_Y;
                }
                if f64::from_bits(xz_scale) == 0.0 {
                    axes &= !(AXIS_X | AXIS_Z);
                }
                for s in shift {
                    axes |= self.axes(s);
                }
                axes
            }
            Node::Shift { kind, .. } => match kind {
                ShiftKind::Shift => 7,
                ShiftKind::A | ShiftKind::B => 5,
            },
            Node::EndIslands => 5,
            Node::DistanceToPoint { .. } | Node::OldBlendedNoise { .. } => 7,
            Node::Gradient { axis, .. } => axis.mask(),
            Node::Slice { axis, input, .. } => self.axes(input) & !axis.mask(),
            Node::FindTopSurface { density, upper_bound, .. } => (self.axes(density) | self.axes(upper_bound)) & !AXIS_Y,
            Node::Prepared(p) => self.prepared[p as usize].axes,
            Node::Spline(_) | Node::Unary(..) | Node::Round { .. } | Node::Binary(..) | Node::Pow(..) | Node::Lerp(..)
            | Node::Clamp { .. } | Node::RangeChoice { .. } | Node::IntervalSelect { .. } | Node::Cache(_)
            | Node::BlendDensity(_) | Node::Interpolated { .. } => {
                let mut axes = 0;
                for c in self.children(id) {
                    axes |= self.axes(c);
                }
                axes
            }
        }
    }
}

fn spline_coordinates(s: &Spline, out: &mut Vec<NodeId>) {
    if let Spline::Multi { coordinate, values, .. } = s {
        out.push(*coordinate);
        for v in values {
            spline_coordinates(v, out);
        }
    }
}

impl Spline {
    /// Rebuilds the spline with every coordinate passed through `f`, in visiting order.
    pub fn map_coordinates(&self, f: &mut impl FnMut(NodeId) -> NodeId) -> Spline {
        match self {
            Spline::Const(b) => Spline::Const(*b),
            Spline::Multi { coordinate, locations, values, derivatives } => {
                let coordinate = f(*coordinate);
                let values = values.iter().map(|v| v.map_coordinates(f)).collect();
                Spline::Multi { coordinate, locations: locations.clone(), values, derivatives: derivatives.clone() }
            }
        }
    }
}

pub fn leaky_relu(negative_factor: f32, input: f32) -> f32 {
    if input > 0.0 { input } else { input * negative_factor }
}

pub fn squeeze(input: f32) -> f32 {
    let clamped = if input < -1.0 { -1.0 } else { super::interval::jmin(input, 1.0) };
    clamped / 2.0 - clamped * clamped * clamped / 24.0
}

fn unary_range(kind: Unary, i: Interval) -> Interval {
    match kind {
        Unary::Abs => Interval::abs(i),
        Unary::Square => Interval::square(i),
        Unary::Cube => Interval::map_monotonic(i, |v| v * v * v),
        Unary::Sqrt => Interval::pow(i, Interval::exact(0.5)),
        Unary::HalfNegative => Interval::map_monotonic(i, |v| leaky_relu(0.5, v)),
        Unary::QuarterNegative => Interval::map_monotonic(i, |v| leaky_relu(0.25, v)),
        Unary::Reciprocal => Interval::reciprocal(i),
        Unary::Negate => Interval::sub(Interval::exact(0.0), i),
        Unary::Squeeze => Interval::map_monotonic(i, squeeze),
        Unary::Log => Interval::log(i),
        Unary::Sign => Interval::sign(i),
    }
}

/// `Math.round(float)`: floor of `x + 1/2`, saturating, NaN to zero.
pub fn java_round(x: f32) -> i32 {
    let f = x.floor();
    (if x - f >= 0.5 { f + 1.0 } else { f }) as i32
}

pub fn round_to_integer(input: f32, kind: RoundKind) -> f32 {
    match kind {
        RoundKind::Floor => input.floor(),
        RoundKind::Round => java_round(input) as f32,
        RoundKind::Ceil => input.ceil(),
        RoundKind::Truncate => {
            if input > 0.0 { input.floor() } else { input.ceil() }
        }
    }
}

/// Reads a `noise/*.json` document.
pub fn parse_noise_params(doc: &Value) -> Result<NoiseParams, TreeError> {
    let first_octave = doc.get("base_octave").and_then(Value::as_i64).ok_or_else(|| TreeError::Invalid("base_octave".into()))?;
    let normalize = match doc.get("normalize") {
        None | Some(Value::Bool(true)) => Normalization::Enabled,
        Some(Value::Bool(false)) => Normalization::Disabled,
        Some(Value::String(s)) if s == "legacy" => Normalization::Legacy,
        other => return invalid(format!("normalize {other:?}")),
    };
    let modifiers: Vec<f64> = match doc.get("amplitude_modifiers") {
        None => Vec::new(),
        Some(Value::Array(a)) => a.iter().map(|v| v.as_f64().ok_or_else(|| TreeError::Invalid("amplitude".into()))).collect::<Result<_, _>>()?,
        other => return invalid(format!("amplitude_modifiers {other:?}")),
    };
    let octave_count = doc.get("octave_count").and_then(Value::as_i64).unwrap_or(1) as i32;
    if !modifiers.is_empty() && modifiers.len() != octave_count as usize {
        return invalid("amplitude_modifiers size differs from octave_count");
    }
    Ok(NoiseParams {
        base_amplitude: doc.get("base_amplitude").and_then(Value::as_f64).unwrap_or(1.0),
        base_octave: first_octave as i32,
        octave_count,
        normalize,
        amplitude_modifiers: modifiers,
    })
}

impl std::fmt::Debug for Tree {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tree").finish_non_exhaustive()
    }
}
