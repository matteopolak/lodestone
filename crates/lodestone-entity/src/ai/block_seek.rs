//! The walk-to-a-block routine several goals share: spiral out from the mob to
//! the nearest cell a predicate accepts, walk to the cell above it, and give up
//! after a fixed number of failed ticks.

use lodestone_model::Vec3;

use super::goal::reduced_tick_delay;
use super::mob::MobController;

const GIVE_UP_TICKS: i32 = 1200;
const STAY_TICKS: i32 = 1200;

/// Whether `cell` is a destination the owning goal wants.
pub type CellFilter<'a> = &'a dyn Fn(&dyn MobController, (i32, i32, i32)) -> bool;

/// The state of one walk toward a matching block.
#[derive(Debug)]
pub struct BlockSeek {
    speed: f64,
    search_range: i32,
    vertical_range: i32,
    vertical_start: i32,
    next_start: i32,
    block: (i32, i32, i32),
    try_ticks: i32,
    max_stay: i32,
    reached: bool,
}

impl BlockSeek {
    /// A seek that spirals out `search_range` blocks and `vertical_range`
    /// layers, walking at `speed`.
    #[must_use]
    pub fn new(speed: f64, search_range: i32, vertical_range: i32) -> Self {
        Self {
            speed,
            search_range,
            vertical_range,
            vertical_start: 0,
            next_start: 0,
            block: (0, 0, 0),
            try_ticks: 0,
            max_stay: 0,
            reached: false,
        }
    }

    /// Starts the vertical scan `start` layers from the mob's feet instead of
    /// at them.
    #[must_use]
    pub fn vertical_start(mut self, start: i32) -> Self {
        self.vertical_start = start;
        self
    }

    /// The block the walk is headed for.
    #[must_use]
    pub fn block(&self) -> (i32, i32, i32) {
        self.block
    }

    /// Whether the mob has come within range of the cell above the block.
    #[must_use]
    pub fn reached(&self) -> bool {
        self.reached
    }

    /// Counts down the restart delay; when it expires, re-arms it with the
    /// usual 200 to 399 ticks and searches.
    pub fn can_use(&mut self, mob: &dyn MobController, valid: CellFilter<'_>, delay: i32) -> bool {
        if self.next_start > 0 {
            self.next_start -= 1;
            return false;
        }
        self.next_start = delay;
        self.find(mob, valid)
    }

    /// The default restart delay for a goal that waits between searches.
    pub fn default_delay(mob: &mut dyn MobController) -> i32 {
        reduced_tick_delay(200 + mob.next_i32(200))
    }

    /// Re-arms the restart delay without counting it down.
    pub fn set_next_start(&mut self, ticks: i32) {
        self.next_start = ticks;
    }

    /// Counts the restart delay down, returning whether it is still running.
    pub fn waiting(&mut self) -> bool {
        if self.next_start > 0 {
            self.next_start -= 1;
            true
        } else {
            false
        }
    }

    /// Spirals out from the mob's feet through the vertical layers and keeps
    /// the nearest cell that is inside the mob's home and accepted by `valid`.
    pub fn find(&mut self, mob: &dyn MobController, valid: CellFilter<'_>) -> bool {
        let here = mob.position();
        let (mx, my, mz) = (here.x.floor() as i32, here.y.floor() as i32, here.z.floor() as i32);
        let step = |n: i32| if n > 0 { -n } else { 1 - n };
        let mut y = self.vertical_start;
        while y <= self.vertical_range {
            for r in 0..self.search_range {
                let mut x = 0;
                while x <= r {
                    let mut z = if x < r && x > -r { r } else { 0 };
                    while z <= r {
                        let cell = (mx + x, my + y - 1, mz + z);
                        if mob.is_cell_within_home(cell) && valid(mob, cell) {
                            self.block = cell;
                            return true;
                        }
                        z = step(z);
                    }
                    x = step(x);
                }
            }
            y = if y > 0 { -y } else { 1 - y };
        }
        false
    }

    /// Whether the walk may go on: not given up, and the block still valid.
    #[must_use]
    pub fn can_continue(&self, mob: &dyn MobController, valid: CellFilter<'_>) -> bool {
        self.try_ticks >= -self.max_stay && self.try_ticks <= GIVE_UP_TICKS && valid(mob, self.block)
    }

    /// Begins the walk.
    pub fn start(&mut self, mob: &mut dyn MobController) {
        let (x, y, z) = self.block;
        mob.move_to(Vec3::new(f64::from(x) + 0.5, f64::from(y + 1), f64::from(z) + 0.5), self.speed);
        self.try_ticks = 0;
        let inner = mob.next_i32(1200) + 1200;
        self.max_stay = mob.next_i32(inner) + STAY_TICKS;
        self.reached = false;
    }

    /// One tick of the walk; `accepted` is how close to the target cell's centre
    /// counts as arrived.
    pub fn tick(&mut self, mob: &mut dyn MobController, accepted: f64) {
        let (x, y, z) = self.block;
        let target = Vec3::new(f64::from(x) + 0.5, f64::from(y + 1), f64::from(z) + 0.5);
        let centre = Vec3::new(target.x, target.y + 0.5, target.z);
        let here = mob.position();
        let d = (centre.x - here.x).powi(2) + (centre.y - here.y).powi(2) + (centre.z - here.z).powi(2);
        if d >= accepted * accepted {
            self.reached = false;
            self.try_ticks += 1;
            if self.try_ticks % 40 == 0 {
                mob.move_to(target, self.speed);
            }
        } else {
            self.reached = true;
            self.try_ticks -= 1;
        }
    }
}
