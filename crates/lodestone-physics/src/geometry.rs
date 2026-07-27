//! `Vec3` and `AABB` mirrors, keeping vanilla's exact `f64` expression order.

/// A double-precision 3-vector, mirroring `net.minecraft.world.phys.Vec3`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vec3d {
    /// X component.
    pub x: f64,
    /// Y component.
    pub y: f64,
    /// Z component.
    pub z: f64,
}

/// A coordinate axis, mirroring `Direction.Axis`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// The X axis.
    X,
    /// The Y axis.
    Y,
    /// The Z axis.
    Z,
}

impl Vec3d {
    /// `Vec3.ZERO`.
    pub const ZERO: Self = Self {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };

    /// Constructs a vector.
    #[must_use]
    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    /// `Vec3.add`.
    #[must_use]
    #[allow(
        clippy::should_implement_trait,
        reason = "named to mirror vanilla `Vec3.add`; expression order is deliberately preserved"
    )]
    pub fn add(self, o: Vec3d) -> Self {
        Self::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }

    /// `Vec3.subtract`.
    #[must_use]
    pub fn subtract(self, o: Vec3d) -> Self {
        Self::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }

    /// `Vec3.scale`.
    #[must_use]
    pub fn scale(self, f: f64) -> Self {
        self.multiply_each(f, f, f)
    }

    /// `Vec3.multiply(double, double, double)`.
    #[must_use]
    pub fn multiply_each(self, fx: f64, fy: f64, fz: f64) -> Self {
        Self::new(self.x * fx, self.y * fy, self.z * fz)
    }

    /// `Vec3.multiply(Vec3)`.
    #[must_use]
    pub fn multiply(self, o: Vec3d) -> Self {
        self.multiply_each(o.x, o.y, o.z)
    }

    /// `Vec3.lengthSqr`.
    #[must_use]
    pub fn length_sqr(self) -> f64 {
        self.x * self.x + self.y * self.y + self.z * self.z
    }

    /// `Vec3.length`.
    #[must_use]
    pub fn length(self) -> f64 {
        self.length_sqr().sqrt()
    }

    /// `Vec3.horizontalDistanceSqr`.
    #[must_use]
    pub fn horizontal_distance_sqr(self) -> f64 {
        self.x * self.x + self.z * self.z
    }

    /// `Vec3.horizontalDistance`.
    #[must_use]
    pub fn horizontal_distance(self) -> f64 {
        (self.x * self.x + self.z * self.z).sqrt()
    }

    /// `Vec3.normalize`.
    ///
    /// Returns `ZERO` when the length is below `1.0E-5F` (a `float` literal in
    /// vanilla, widened to `double` for the comparison), exactly as vanilla.
    #[must_use]
    pub fn normalize(self) -> Self {
        let len = (self.x * self.x + self.y * self.y + self.z * self.z).sqrt();
        if len < f64::from(1.0e-5f32) {
            Self::ZERO
        } else {
            Self::new(self.x / len, self.y / len, self.z / len)
        }
    }

    /// `Vec3.get(Axis)`.
    #[must_use]
    pub fn get(self, axis: Axis) -> f64 {
        match axis {
            Axis::X => self.x,
            Axis::Y => self.y,
            Axis::Z => self.z,
        }
    }

    /// `Vec3.with(Axis, double)`.
    #[must_use]
    pub fn with(self, axis: Axis, value: f64) -> Self {
        match axis {
            Axis::X => Self::new(value, self.y, self.z),
            Axis::Y => Self::new(self.x, value, self.z),
            Axis::Z => Self::new(self.x, self.y, value),
        }
    }
}

/// An axis-aligned bounding box, mirroring `net.minecraft.world.phys.AABB`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    /// Minimum X.
    pub min_x: f64,
    /// Minimum Y.
    pub min_y: f64,
    /// Minimum Z.
    pub min_z: f64,
    /// Maximum X.
    pub max_x: f64,
    /// Maximum Y.
    pub max_y: f64,
    /// Maximum Z.
    pub max_z: f64,
}

impl Aabb {
    /// Constructs a box from explicit bounds.
    #[must_use]
    pub const fn new(
        min_x: f64,
        min_y: f64,
        min_z: f64,
        max_x: f64,
        max_y: f64,
        max_z: f64,
    ) -> Self {
        Self {
            min_x,
            min_y,
            min_z,
            max_x,
            max_y,
            max_z,
        }
    }

    /// `AABB.min(Axis)`.
    #[must_use]
    pub fn min(&self, axis: Axis) -> f64 {
        match axis {
            Axis::X => self.min_x,
            Axis::Y => self.min_y,
            Axis::Z => self.min_z,
        }
    }

    /// `AABB.max(Axis)`.
    #[must_use]
    pub fn max(&self, axis: Axis) -> f64 {
        match axis {
            Axis::X => self.max_x,
            Axis::Y => self.max_y,
            Axis::Z => self.max_z,
        }
    }

    /// `AABB.move(double, double, double)`.
    #[must_use]
    pub fn moved(&self, xa: f64, ya: f64, za: f64) -> Self {
        Self::new(
            self.min_x + xa,
            self.min_y + ya,
            self.min_z + za,
            self.max_x + xa,
            self.max_y + ya,
            self.max_z + za,
        )
    }

    /// `AABB.move(Vec3)`.
    #[must_use]
    pub fn move_vec(&self, v: Vec3d) -> Self {
        self.moved(v.x, v.y, v.z)
    }

    /// `AABB.expandTowards(double, double, double)` — the broadphase sweep box.
    #[must_use]
    pub fn expand_towards(&self, xa: f64, ya: f64, za: f64) -> Self {
        let mut min_x = self.min_x;
        let mut min_y = self.min_y;
        let mut min_z = self.min_z;
        let mut max_x = self.max_x;
        let mut max_y = self.max_y;
        let mut max_z = self.max_z;
        if xa < 0.0 {
            min_x += xa;
        } else if xa > 0.0 {
            max_x += xa;
        }
        if ya < 0.0 {
            min_y += ya;
        } else if ya > 0.0 {
            max_y += ya;
        }
        if za < 0.0 {
            min_z += za;
        } else if za > 0.0 {
            max_z += za;
        }
        Self::new(min_x, min_y, min_z, max_x, max_y, max_z)
    }

    /// `AABB.setMinY`.
    #[must_use]
    pub fn with_min_y(&self, min_y: f64) -> Self {
        Self::new(
            self.min_x, min_y, self.min_z, self.max_x, self.max_y, self.max_z,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_towards_matches_vanilla_sign_rules() {
        let b = Aabb::new(0.0, 0.0, 0.0, 1.0, 1.0, 1.0);
        let e = b.expand_towards(-0.5, 2.0, 0.0);
        assert_eq!(e, Aabb::new(-0.5, 0.0, 0.0, 1.0, 3.0, 1.0));
    }

    #[test]
    fn normalize_below_threshold_is_zero() {
        let v = Vec3d::new(1.0e-6, 0.0, 0.0);
        assert_eq!(v.normalize(), Vec3d::ZERO);
    }

    #[test]
    fn horizontal_distance_ignores_y() {
        let v = Vec3d::new(3.0, 100.0, 4.0);
        assert_eq!(v.horizontal_distance(), 5.0);
    }
}
