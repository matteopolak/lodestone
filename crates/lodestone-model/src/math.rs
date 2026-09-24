use std::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Sub, SubAssign};

const SECTION_SIZE: i32 = 16;

/// A three-dimensional vector using 64-bit floating point coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec3 {
    /// X coordinate.
    pub x: f64,
    /// Y coordinate.
    pub y: f64,
    /// Z coordinate.
    pub z: f64,
}

impl Vec3 {
    /// Creates a vector from components.
    #[must_use]
    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    /// Returns this vector scaled by `factor`.
    #[must_use]
    pub const fn scale(self, factor: f64) -> Self {
        Self::new(self.x * factor, self.y * factor, self.z * factor)
    }

    /// Returns this vector's Euclidean length.
    #[must_use]
    pub fn length(self) -> f64 {
        self.dot(self).sqrt()
    }

    /// Returns a unit vector in this vector's direction.
    ///
    /// Returns the zero vector unchanged when called on the zero vector.
    #[must_use]
    pub fn normalize(self) -> Self {
        let length = self.length();
        if length == 0.0 {
            Self::default()
        } else {
            self / length
        }
    }

    /// Returns the dot product of this vector and `other`.
    #[must_use]
    pub const fn dot(self, other: Self) -> f64 {
        self.x
            .mul_add(other.x, self.y.mul_add(other.y, self.z * other.z))
    }
}

impl Add for Vec3 {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self::new(self.x + rhs.x, self.y + rhs.y, self.z + rhs.z)
    }
}

impl AddAssign for Vec3 {
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

impl Sub for Vec3 {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self::new(self.x - rhs.x, self.y - rhs.y, self.z - rhs.z)
    }
}

impl SubAssign for Vec3 {
    fn sub_assign(&mut self, rhs: Self) {
        *self = *self - rhs;
    }
}

impl Mul<f64> for Vec3 {
    type Output = Self;

    fn mul(self, rhs: f64) -> Self::Output {
        self.scale(rhs)
    }
}

impl MulAssign<f64> for Vec3 {
    fn mul_assign(&mut self, rhs: f64) {
        *self = *self * rhs;
    }
}

impl Div<f64> for Vec3 {
    type Output = Self;

    fn div(self, rhs: f64) -> Self::Output {
        Self::new(self.x / rhs, self.y / rhs, self.z / rhs)
    }
}

impl DivAssign<f64> for Vec3 {
    fn div_assign(&mut self, rhs: f64) {
        *self = *self / rhs;
    }
}

/// A three-dimensional vector using 32-bit floating point coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec3f {
    /// X coordinate.
    pub x: f32,
    /// Y coordinate.
    pub y: f32,
    /// Z coordinate.
    pub z: f32,
}

impl Vec3f {
    /// Creates a vector from components.
    #[must_use]
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    /// Returns this vector scaled by `factor`.
    #[must_use]
    pub const fn scale(self, factor: f32) -> Self {
        Self::new(self.x * factor, self.y * factor, self.z * factor)
    }

    /// Returns this vector's Euclidean length.
    #[must_use]
    pub fn length(self) -> f32 {
        self.dot(self).sqrt()
    }

    /// Returns a unit vector in this vector's direction.
    ///
    /// Returns the zero vector unchanged when called on the zero vector.
    #[must_use]
    pub fn normalize(self) -> Self {
        let length = self.length();
        if length == 0.0 {
            Self::default()
        } else {
            self / length
        }
    }

    /// Returns the dot product of this vector and `other`.
    #[must_use]
    pub const fn dot(self, other: Self) -> f32 {
        self.x
            .mul_add(other.x, self.y.mul_add(other.y, self.z * other.z))
    }
}

impl Add for Vec3f {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self::new(self.x + rhs.x, self.y + rhs.y, self.z + rhs.z)
    }
}

impl AddAssign for Vec3f {
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

impl Sub for Vec3f {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self::new(self.x - rhs.x, self.y - rhs.y, self.z - rhs.z)
    }
}

impl SubAssign for Vec3f {
    fn sub_assign(&mut self, rhs: Self) {
        *self = *self - rhs;
    }
}

impl Mul<f32> for Vec3f {
    type Output = Self;

    fn mul(self, rhs: f32) -> Self::Output {
        self.scale(rhs)
    }
}

impl MulAssign<f32> for Vec3f {
    fn mul_assign(&mut self, rhs: f32) {
        *self = *self * rhs;
    }
}

impl Div<f32> for Vec3f {
    type Output = Self;

    fn div(self, rhs: f32) -> Self::Output {
        Self::new(self.x / rhs, self.y / rhs, self.z / rhs)
    }
}

impl DivAssign<f32> for Vec3f {
    fn div_assign(&mut self, rhs: f32) {
        *self = *self / rhs;
    }
}

/// A block position in world coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct BlockPos {
    /// X block coordinate.
    pub x: i32,
    /// Y block coordinate.
    pub y: i32,
    /// Z block coordinate.
    pub z: i32,
}

impl BlockPos {
    /// Creates a block position from coordinates.
    #[must_use]
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    /// Returns the chunk containing this block.
    ///
    /// Minecraft chunk coordinates floor-divide by sixteen, so negative block
    /// coordinates map to the chunk below zero rather than truncating toward it.
    #[must_use]
    pub const fn chunk_pos(self) -> ChunkPos {
        ChunkPos::new(floor_div_16(self.x), floor_div_16(self.z))
    }

    /// Returns the section containing this block.
    ///
    /// Section coordinates floor-divide all three block axes by sixteen.
    #[must_use]
    pub const fn section_pos(self) -> SectionPos {
        SectionPos::new(
            floor_div_16(self.x),
            floor_div_16(self.y),
            floor_div_16(self.z),
        )
    }
}

impl From<BlockPos> for ChunkPos {
    fn from(value: BlockPos) -> Self {
        value.chunk_pos()
    }
}

impl From<BlockPos> for SectionPos {
    fn from(value: BlockPos) -> Self {
        value.section_pos()
    }
}

/// A chunk position in chunk coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ChunkPos {
    /// X chunk coordinate.
    pub x: i32,
    /// Z chunk coordinate.
    pub z: i32,
}

impl ChunkPos {
    /// Creates a chunk position from coordinates.
    #[must_use]
    pub const fn new(x: i32, z: i32) -> Self {
        Self { x, z }
    }

    /// Returns the minimum block position covered by this chunk at world y=0.
    #[must_use]
    pub const fn block_min(self) -> BlockPos {
        BlockPos::new(self.x * SECTION_SIZE, 0, self.z * SECTION_SIZE)
    }
}

/// A section position in section coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SectionPos {
    /// X section coordinate.
    pub x: i32,
    /// Y section coordinate.
    pub y: i32,
    /// Z section coordinate.
    pub z: i32,
}

impl SectionPos {
    /// Creates a section position from coordinates.
    #[must_use]
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    /// Returns the chunk containing this section.
    #[must_use]
    pub const fn chunk_pos(self) -> ChunkPos {
        ChunkPos::new(self.x, self.z)
    }

    /// Returns the minimum block position covered by this section.
    #[must_use]
    pub const fn block_min(self) -> BlockPos {
        BlockPos::new(
            self.x * SECTION_SIZE,
            self.y * SECTION_SIZE,
            self.z * SECTION_SIZE,
        )
    }
}

impl From<SectionPos> for ChunkPos {
    fn from(value: SectionPos) -> Self {
        value.chunk_pos()
    }
}

const fn floor_div_16(value: i32) -> i32 {
    value.div_euclid(SECTION_SIZE)
}

/// Yaw and pitch in degrees.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rotation {
    /// Horizontal rotation.
    pub yaw: f32,
    /// Vertical rotation.
    pub pitch: f32,
}

impl Rotation {
    /// Creates a rotation from yaw and pitch.
    #[must_use]
    pub const fn new(yaw: f32, pitch: f32) -> Self {
        Self { yaw, pitch }
    }
}

/// A rotation quaternion, kept as raw `(x, y, z, w)` components rather than
/// pulling a real quaternion-math crate (`glam`) into this dependency-light
/// model crate — `lodestone_model::Vec3`/`Vec3f` already make the same choice
/// for plain vectors. This exists for the wire's `QUATERNION` metadata
/// serializer (`Display.DATA_LEFT_ROTATION_ID`/`DATA_RIGHT_ROTATION_ID`,
/// `26.2`): a version adapter decodes `x, y, z, w` in that order (matching
/// `FriendlyByteBuf.readQuaternion`'s `new Quaternionf(x, y, z, w)`) and hands
/// it through unmodified. A consumer that already depends on `glam` converts
/// with `glam::Quat::from_xyzw(q.x, q.y, q.z, q.w)` — the field order is
/// identical, so this is a relabelling, not a transform.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quat {
    /// X component.
    pub x: f32,
    /// Y component.
    pub y: f32,
    /// Z component.
    pub z: f32,
    /// W (scalar) component.
    pub w: f32,
}

impl Quat {
    /// Creates a quaternion from raw components, in wire order.
    #[must_use]
    pub const fn new(x: f32, y: f32, z: f32, w: f32) -> Self {
        Self { x, y, z, w }
    }

    /// The identity rotation — vanilla's own identity transformation's
    /// left/right rotation and the corresponding metadata accessors' own defaults.
    pub const IDENTITY: Self = Self::new(0.0, 0.0, 0.0, 1.0);
}

impl Default for Quat {
    /// **Not** `(0, 0, 0, 0)` — a quaternion's meaningful "no rotation" value
    /// is [`Self::IDENTITY`], so `Default` returns that rather than the
    /// all-zero value `#[derive(Default)]` would give every other field-`0`
    /// type here. A caller that wants the derived all-zero value almost
    /// certainly wanted `IDENTITY` instead; giving them the same value either
    /// way removes the trap.
    fn default() -> Self {
        Self::IDENTITY
    }
}
