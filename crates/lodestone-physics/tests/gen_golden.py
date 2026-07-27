#!/usr/bin/env python3
"""Independent ground-truth oracle for lodestone-physics golden traces.

This is a *separate* re-implementation of vanilla's player movement, written in
Python (whose ``float`` is IEEE-754 binary64, with ``struct`` used for the
binary32 casts vanilla performs). It is deliberately independent of the Rust
crate so that a shared conceptual bug is less likely to hide behind a test that
checks the code against itself.

It emits ``golden_traces.rs`` containing, for each scenario, the per-tick
position and velocity as raw ``f64`` bit patterns (so no decimal rounding can
creep in). The Rust integration test replays the same scenarios through the
crate and asserts bit-for-bit equality.

Confidence: the free-fall trace it produces begins with the widely-documented
vanilla sequence (dy = 0, -0.0784, -0.155232, -0.23052736, ...), which is an
external anchor independent of this script's structure.
"""
import math
import struct


def f32(x):
    return struct.unpack("f", struct.pack("f", float(x)))[0]


# ---- Mth sine LUT (same construction as vanilla Mth.SIN) --------------------
SIN_SCALE = 10430.378350470453
_SIN = [f32(math.sin(i / SIN_SCALE)) for i in range(65536)]


def mth_sin(v):
    return _SIN[int(v * SIN_SCALE) & 65535]


def mth_cos(v):
    return _SIN[int(v * SIN_SCALE + 16384.0) & 65535]


def mth_floor(v):
    return math.floor(v)


def mth_ceil(v):
    return math.ceil(v)


# `Direction.Plane.HORIZONTAL` order: NORTH(-Z), EAST(+X), SOUTH(+Z), WEST(-X).
HORIZONTAL = [(0, -1), (1, 0), (0, 1), (-1, 0)]


def vec_normalize(v):
    # Vec3.normalize(): compare length against the float literal 1.0E-5F widened.
    d = math.sqrt(v[0] * v[0] + v[1] * v[1] + v[2] * v[2])
    if d < float(f32(1.0e-5)):
        return [0.0, 0.0, 0.0]
    return [v[0] / d, v[1] / d, v[2] / d]


def own_height(amount):
    # FlowingFluid.getOwnHeight() = amount / 9.0F (float).
    return f32(f32(amount) / f32(9.0))


def affects_flow(cell, kind):
    # FlowingFluid.affectsFlow(): empty neighbour or same fluid type.
    return cell is None or cell[0] == kind


def neighbour_own_height(cell, kind):
    if cell is not None and cell[0] == kind:
        return own_height(cell[1])
    return f32(0.0)


def get_flow(world, x, y, z, cell):
    # FlowingFluid.getFlow(level, pos, fluidState). See Rust `fluid::get_flow`.
    kind = cell[0]
    this_height = own_height(cell[1])
    flow_x = 0.0
    flow_z = 0.0
    for dx, dz in HORIZONTAL:
        nx, nz = x + dx, z + dz
        nb = world.fluid_at(nx, y, nz)
        if not affects_flow(nb, kind):
            continue
        nh = neighbour_own_height(nb, kind)
        distance = f32(0.0)
        if nh == 0.0:
            if not world.blocks_motion(nx, y, nz):
                below = world.fluid_at(nx, y - 1, nz)
                if affects_flow(below, kind):
                    bh = neighbour_own_height(below, kind)
                    if bh > 0.0:
                        distance = f32(this_height - f32(bh - f32(0.8888889)))
        elif nh > 0.0:
            distance = f32(this_height - nh)
        if distance != 0.0:
            flow_x += float(f32(f32(dx) * distance))
            flow_z += float(f32(f32(dz) * distance))
    flow = [flow_x, 0.0, flow_z]
    if cell[2]:  # FALLING
        for dx, dz in HORIZONTAL:
            nx, nz = x + dx, z + dz
            if world.is_solid_face(nx, y, nz) or world.is_solid_face(nx, y + 1, nz):
                flow = vec_normalize(flow)
                flow = [flow[0], flow[1] - 6.0, flow[2]]
                break
    return vec_normalize(flow)


def apply_fluid_push(world, s, kind, scale):
    # Entity.updateFluidInteraction -> EntityFluidInteraction.update/applyCurrentTo.
    bb = bounding_box(s)
    d = 0.001
    box_min_x, box_min_y, box_min_z = bb[0] + d, bb[1] + d, bb[2] + d
    box_max_x, box_max_y, box_max_z = bb[3] - d, bb[4] - d, bb[5] - d
    x0, y0, z0 = mth_floor(box_min_x), mth_floor(box_min_y), mth_floor(box_min_z)
    x1, y1, z1 = mth_ceil(box_max_x) - 1, mth_ceil(box_max_y) - 1, mth_ceil(box_max_z) - 1
    entity_y = bb[1]
    height = 0.0
    acc = [0.0, 0.0, 0.0]
    count = 0
    for x in range(x0, x1 + 1):
        for y in range(y0, y1 + 1):
            for z in range(z0, z1 + 1):
                cell = world.fluid_at(x, y, z)
                if cell is None or cell[0] != kind:
                    continue
                above = world.fluid_at(x, y + 1, z)
                same_above = above is not None and above[0] == kind
                cell_height = f32(1.0) if same_above else own_height(cell[1])
                fluid_top = float(y) + float(cell_height)
                if fluid_top < box_min_y:
                    continue
                height = max(fluid_top - entity_y, height)
                flow = get_flow(world, x, y, z, cell)
                if height < 0.4:
                    flow = [flow[0] * height, flow[1] * height, flow[2] * height]
                acc = [acc[0] + flow[0], acc[1] + flow[1], acc[2] + flow[2]]
                count += 1
    if count == 0:
        return
    if (acc[0] * acc[0] + acc[1] * acc[1] + acc[2] * acc[2]) < float(f32(1.0e-5)):
        return
    inv = 1.0 / count
    impulse = [acc[0] * inv, acc[1] * inv, acc[2] * inv]
    old = s.vel
    impulse = [impulse[0] * scale, impulse[1] * scale, impulse[2] * scale]
    ilen = math.sqrt(impulse[0] ** 2 + impulse[1] ** 2 + impulse[2] ** 2)
    floor = 0.0045000000000000005
    if abs(old[0]) < 0.003 and abs(old[2]) < 0.003 and ilen < floor:
        n = vec_normalize(impulse)
        impulse = [n[0] * floor, n[1] * floor, n[2] * floor]
    s.vel[0] += impulse[0]
    s.vel[1] += impulse[1]
    s.vel[2] += impulse[2]


def clamp_f32(v, lo, hi):
    v = f32(v)
    if v < lo:
        return lo
    return f32(min(v, hi)) if not math.isnan(v) else float("nan")


def compute_modified_friction(friction, modifier):
    return clamp_f32(f32(1.0 - f32(f32(1.0 - friction) * modifier)), 0.0, 1.0)


# ---- Profile (mc 1.21 / 26.2) ----------------------------------------------
class Profile:
    base_movement_speed = f32(0.1)
    sprint_speed_modifier = f32(0.3)
    sneaking_speed = f32(0.3)
    gravity = f32(0.08)
    air_drag = f32(0.91)
    vertical_air_drag = f32(0.98)
    air_drag_modifier = f32(1.0)
    friction_modifier = f32(1.0)
    ground_accel = f32(0.21600002)
    flying_speed = f32(0.02)
    jump_power = f32(0.42)
    sprint_jump_boost = 0.2
    step_height = f32(0.6)
    width = f32(0.6)
    height = f32(1.8)
    water_slow_down = f32(0.8)
    water_sprint_slow_down = f32(0.9)
    fluid_input_speed = f32(0.02)


P = Profile()


# ---- World model (mirrors the Rust TestWorld) ------------------------------
class World:
    def __init__(self):
        self.solid = set()
        self.boxes = {}      # (x,y,z) -> list of world-space aabbs
        self.friction = {}   # (x,y,z) -> f32
        self.water = set()   # (x,y,z) water cells
        self.climbable = set()  # (x,y,z) ladder/vine cells
        self.lava = set()    # (x,y,z) lava cells
        self.slime = set()   # (x,y,z) slime cells (bounce_restitution 1.0)
        self.jumpf = {}      # (x,y,z) -> jump factor (honey 0.5)
        self.speedf = {}     # (x,y,z) -> speed factor (soul sand / honey 0.4)
        self.fluids = {}     # (x,y,z) -> ("water"|"lava", amount 1..8, falling)

    def add_slime(self, x, y, z):
        self.slime.add((x, y, z))
        return self

    def bounce_restitution(self, x, y, z):
        # Slime = 1.0F; player (LivingEntity) does not get the non-living *0.8.
        return f32(1.0) if (x, y, z) in self.slime else f32(0.0)

    def set_jump_factor(self, x, y, z, v):
        self.jumpf[(x, y, z)] = f32(v)
        return self

    def jump_factor(self, x, y, z):
        return self.jumpf.get((x, y, z), f32(1.0))

    def set_speed_factor(self, x, y, z, v):
        self.speedf[(x, y, z)] = f32(v)
        return self

    def speed_factor(self, x, y, z):
        return self.speedf.get((x, y, z), f32(1.0))

    def add_lava(self, x, y, z):
        self.lava.add((x, y, z))
        return self

    def is_lava(self, x, y, z):
        return (x, y, z) in self.lava

    def add_climbable(self, x, y, z):
        self.climbable.add((x, y, z))
        return self

    def is_climbable(self, x, y, z):
        return (x, y, z) in self.climbable

    def add_solid(self, x, y, z):
        self.solid.add((x, y, z))
        return self

    def add_water(self, x, y, z):
        self.water.add((x, y, z))
        return self

    def add_water_cell(self, x, y, z, amount, falling=False):
        # Water that also participates in flow-current (fluid-push): registers in
        # both `water` (for the is_in_water dispatch/submersion) and `fluids`.
        self.water.add((x, y, z))
        self.fluids[(x, y, z)] = ("water", amount, falling)
        return self

    def add_lava_cell(self, x, y, z, amount, falling=False):
        self.lava.add((x, y, z))
        self.fluids[(x, y, z)] = ("lava", amount, falling)
        return self

    def fluid_at(self, x, y, z):
        return self.fluids.get((x, y, z))

    def blocks_motion(self, x, y, z):
        return (x, y, z) in self.solid

    def is_solid_face(self, x, y, z):
        return False

    def is_water(self, x, y, z):
        return (x, y, z) in self.water

    def add_box(self, x, y, z, local):
        w = (
            local[0] + x, local[1] + y, local[2] + z,
            local[3] + x, local[4] + y, local[5] + z,
        )
        self.boxes.setdefault((x, y, z), []).append(w)
        return self

    def set_friction(self, x, y, z, fr):
        self.friction[(x, y, z)] = f32(fr)
        return self

    def collision_boxes(self, x, y, z, out):
        if (x, y, z) in self.boxes:
            out.extend(self.boxes[(x, y, z)])
        elif (x, y, z) in self.solid:
            out.append((x, y, z, x + 1.0, y + 1.0, z + 1.0))

    def get_friction(self, x, y, z):
        return self.friction.get((x, y, z), f32(0.6))


EPS = 1.0e-7


def gather(world, region):
    minx = math.floor(region[0]) - 1
    miny = math.floor(region[1]) - 1
    minz = math.floor(region[2]) - 1
    maxx = math.floor(region[3]) + 1
    maxy = math.floor(region[4]) + 1
    maxz = math.floor(region[5]) + 1
    out = []
    for x in range(minx, maxx + 1):
        for y in range(miny, maxy + 1):
            for z in range(minz, maxz + 1):
                world.collision_boxes(x, y, z, out)
    return out


def expand_towards(bb, xa, ya, za):
    minx, miny, minz, maxx, maxy, maxz = bb
    if xa < 0.0:
        minx += xa
    elif xa > 0.0:
        maxx += xa
    if ya < 0.0:
        miny += ya
    elif ya > 0.0:
        maxy += ya
    if za < 0.0:
        minz += za
    elif za > 0.0:
        maxz += za
    return (minx, miny, minz, maxx, maxy, maxz)


def bb_min(bb, axis):
    return bb[axis]


def bb_max(bb, axis):
    return bb[axis + 3]


def collide_one(axis, shape, moving, distance):
    a = axis
    b, c = [ax for ax in (0, 1, 2) if ax != axis]
    if not (bb_max(moving, b) - EPS > bb_min(shape, b) and bb_min(moving, b) + EPS < bb_max(shape, b)):
        return distance
    if not (bb_max(moving, c) - EPS > bb_min(shape, c) and bb_min(moving, c) + EPS < bb_max(shape, c)):
        return distance
    maxa = bb_max(moving, a)
    mina = bb_min(moving, a)
    if distance > 0.0:
        if maxa - EPS <= bb_min(shape, a):
            nd = bb_min(shape, a) - maxa
            if nd >= -EPS:
                return min(distance, nd)
    elif distance < 0.0:
        if mina + EPS >= bb_max(shape, a):
            nd = bb_max(shape, a) - mina
            if nd <= EPS:
                return max(distance, nd)
    return distance


def collide_axis(axis, moving, shapes, distance):
    for shape in shapes:
        if abs(distance) < EPS:
            return 0.0
        distance = collide_one(axis, shape, moving, distance)
    return distance


def bb_move(bb, dx, dy, dz):
    return (bb[0] + dx, bb[1] + dy, bb[2] + dz, bb[3] + dx, bb[4] + dy, bb[5] + dz)


def axis_step_order(mx, mz):
    return [1, 2, 0] if abs(mx) < abs(mz) else [1, 0, 2]


def collide_with_shapes(mv, bb, shapes):
    if not shapes:
        return mv
    resolved = [0.0, 0.0, 0.0]
    for axis in axis_step_order(mv[0], mv[2]):
        am = mv[axis]
        if am != 0.0:
            moved = bb_move(bb, resolved[0], resolved[1], resolved[2])
            resolved[axis] = collide_axis(axis, moved, shapes, am)
    return tuple(resolved)


def candidate_step_heights(bb, colliders, max_step, skip):
    cands = []
    for col in colliders:
        broke = False
        for coord in (col[1], col[4]):
            rel = f32(coord - bb[1])
            if rel >= 0.0 and rel != skip:
                if rel > max_step:
                    broke = True
                    break
                if rel not in cands:
                    cands.append(rel)
        if broke:
            continue
    cands.sort()
    return cands


def hdist_sqr(v):
    return v[0] * v[0] + v[2] * v[2]


def collide(world, mv, bb, on_ground, max_step):
    lensq = mv[0] * mv[0] + mv[1] * mv[1] + mv[2] * mv[2]
    if lensq == 0.0:
        step = mv
    else:
        region = expand_towards(bb, mv[0], mv[1], mv[2])
        shapes = gather(world, region)
        step = collide_with_shapes(mv, bb, shapes)
    xcol = mv[0] != step[0]
    ycol = mv[1] != step[1]
    zcol = mv[2] != step[2]
    on_ground_after = ycol and mv[1] < 0.0
    if max_step > 0.0 and (on_ground_after or on_ground) and (xcol or zcol):
        grounded = bb_move(bb, 0.0, step[1], 0.0) if on_ground_after else bb
        step_box = expand_towards(grounded, mv[0], float(max_step), mv[2])
        if not on_ground_after:
            step_box = expand_towards(step_box, 0.0, -EPS, 0.0)
        colliders = gather(world, step_box)
        skip = f32(step[1])
        for cand in candidate_step_heights(grounded, colliders, max_step, skip):
            s = collide_with_shapes((mv[0], float(cand), mv[2]), grounded, colliders)
            if hdist_sqr(s) > hdist_sqr(step):
                dtg = bb[1] - grounded[1]
                return (s[0], s[1] - dtg, s[2])
    return step


# ---- Player tick (mirrors player.rs tick_air) ------------------------------
class State:
    def __init__(self, x, y, z, yaw):
        self.pos = [x, y, z]
        self.vel = [0.0, 0.0, 0.0]
        self.yaw = f32(yaw)
        self.pitch = f32(0.0)
        self.fall_flying = False
        self.on_ground = False
        self.hcol = False
        self.no_jump_delay = 0
        self.sprinting = False
        # Physics-affecting status effects (mirrors StatusEffects).
        self.levitation = None   # None or amplifier (0-based)
        self.slow_falling = False
        self.dolphins_grace = False
        self.jump_boost = None   # None or amplifier (0-based)


def bounding_box(s):
    half = P.width / 2.0
    return (s.pos[0] - half, s.pos[1], s.pos[2] - half,
            s.pos[0] + half, s.pos[1] + P.height, s.pos[2] + half)


def player_speed(sprinting):
    base = float(P.base_movement_speed)
    if sprinting:
        return f32(base * (1.0 + float(P.sprint_speed_modifier)))
    return f32(base)


def modify_input(strafe, forward, sneak):
    if strafe * strafe + forward * forward == 0.0:
        return (strafe, forward)
    sx = f32(strafe * f32(0.98))
    sy = f32(forward * f32(0.98))
    if sneak:
        sx = f32(sx * P.sneaking_speed)
        sy = f32(sy * P.sneaking_speed)
    length = f32(math.sqrt(f32(f32(sx * sx) + f32(sy * sy))))
    if length <= 0.0:
        return (sx, sy)
    dx = f32(sx / length)
    dy = f32(sy / length)
    ax = abs(dx)
    ay = abs(dy)
    tan = f32(ax / ay) if ay > ax else f32(ay / ax)
    dtus = f32(math.sqrt(f32(1.0 + f32(tan * tan))))
    ml = min(f32(length * dtus), 1.0)
    return (f32(dx * ml), f32(dy * ml))


def input_vector(strafe, forward, speed, yaw):
    lensq = strafe * strafe + forward * forward
    if lensq < 1.0e-7:
        return (0.0, 0.0, 0.0)
    if lensq > 1.0:
        d = math.sqrt(lensq)
        if d < float(f32(1.0e-5)):
            ix, iz = 0.0, 0.0
        else:
            ix, iz = strafe / d, forward / d
    else:
        ix, iz = strafe, forward
    mx = ix * speed
    mz = iz * speed
    rad = f32(yaw * f32(math.pi / 180.0))
    s = float(mth_sin(rad))
    c = float(mth_cos(rad))
    return (mx * c - mz * s, 0.0, mz * c + mx * s)


def friction_block(pos):
    return (mth_floor(pos[0]), mth_floor(pos[1] - float(f32(0.500001))), mth_floor(pos[2]))


def restitute(world, s, delta, resolved, xcol, zcol, vcol, vbelow, suppress_bounce):
    restitution = 0.0  # getEntityBounciness() == 0.0 for a player
    vx, vy, vz = delta[0], delta[1], delta[2]
    if xcol:
        vx = -delta[0] * restitution
    if zcol:
        vz = -delta[2] * restitution
    if vcol:
        if vbelow:
            ex = mth_floor(s.pos[0])
            ey = mth_floor(s.pos[1] - float(f32(0.2)))
            ez = mth_floor(s.pos[2])
            block_b = float(world.bounce_restitution(ex, ey, ez))
            eg = effective_gravity(float(P.gravity), delta[1] <= 0.0, s.slow_falling)
            if (not (-delta[1] < eg)) and (not suppress_bounce):
                restitution = max(restitution, block_b)
            else:
                restitution = 0.0
        if restitution > 0.0:
            portion = resolved[1] / delta[1]
            eg = effective_gravity(float(P.gravity), delta[1] <= 0.0, s.slow_falling)
            grav_comp = portion * eg
            eff_drag = 1.0 + portion * (float(f32(0.98)) - 1.0)  # lerp(portion,1,0.98)
        else:
            grav_comp = 0.0
            eff_drag = 1.0
        vy = (grav_comp - delta[1]) * eff_drag * restitution
    return [vx, vy, vz]


def do_move(world, s, delta, suppress_bounce=False):
    bb = bounding_box(s)
    resolved = collide(world, tuple(delta), bb, s.on_ground, float(P.step_height))
    xcol = abs(delta[0] - resolved[0]) >= float(f32(1.0e-5))
    zcol = abs(delta[2] - resolved[2]) >= float(f32(1.0e-5))
    s.hcol = xcol or zcol
    moved_vert = abs(delta[1]) > 0.0
    vcol = delta[1] != resolved[1]
    vbelow = vcol and delta[1] < 0.0
    s.on_ground = vbelow
    lensq_r = resolved[0]**2 + resolved[1]**2 + resolved[2]**2
    lensq_d = delta[0]**2 + delta[1]**2 + delta[2]**2
    if lensq_r > 1.0e-7 or lensq_d - lensq_r < 1.0e-7:
        s.pos[0] += resolved[0]
        s.pos[1] += resolved[1]
        s.pos[2] += resolved[2]
    # deltaMovement stays == delta through collide; restitute rewrites it
    # (zeroing a blocked axis or bouncing off a bouncy block).
    if (moved_vert and vcol) or s.hcol:
        s.vel = restitute(world, s, delta, resolved, xcol, zcol, vcol, vbelow, suppress_bounce)
    else:
        s.vel = [delta[0], delta[1], delta[2]]
    # block speed factor (soul sand / honey), applied last as in move().
    # blockPosition() (floor of feet) first; fall to the block below only if 1.0.
    bx, by, bz = mth_floor(s.pos[0]), mth_floor(s.pos[1]), mth_floor(s.pos[2])
    sf = float(world.speed_factor(bx, by, bz))
    if sf == 1.0:
        fx, fy, fz = friction_block(s.pos)
        bsf = float(world.speed_factor(fx, fy, fz))
    else:
        bsf = sf
    s.vel = [s.vel[0] * bsf, s.vel[1] * 1.0, s.vel[2] * bsf]


def clamp_f64(value, lo, hi):
    # Mth.clamp(double, double, double): value < min ? min : Math.min(value, max)
    if value < lo:
        return lo
    return min(value, hi)


def handle_on_climbable(vel, sneak):
    # Bounds are the float literals -0.15F/0.15F promoted to double.
    bound = f32(0.15)
    xd = clamp_f64(vel[0], -bound, bound)
    zd = clamp_f64(vel[2], -bound, bound)
    yd = max(vel[1], -bound)
    if yd < 0.0 and sneak:
        yd = 0.0
    return [xd, yd, zd]


def jump_boost_power(jump_boost):
    if jump_boost is None:
        return f32(0.0)
    return f32(f32(0.1) * f32(float(jump_boost) + 1.0))


def block_jump_factor(world, pos):
    here = world.jump_factor(mth_floor(pos[0]), mth_floor(pos[1]), mth_floor(pos[2]))
    if here == 1.0:
        bx, by, bz = friction_block(pos)
        return world.jump_factor(bx, by, bz)
    return here


def jump_from_ground(world, s):
    bjf = block_jump_factor(world, s.pos)
    jp = f32(f32(f32(P.jump_power) * bjf) + jump_boost_power(s.jump_boost))
    if jp <= 1.0e-5:
        return
    s.vel[1] = max(float(jp), s.vel[1])
    if s.sprinting:
        angle = f32(s.yaw * f32(math.pi / 180.0))
        b = P.sprint_jump_boost
        s.vel[0] += float(-mth_sin(angle)) * b
        s.vel[2] += float(mth_cos(angle)) * b


def tick_air(world, s, forward, strafe, jump, sneak, sprint):
    if s.no_jump_delay > 0:
        s.no_jump_delay -= 1
    dx, dy, dz = s.vel
    if (s.vel[0]**2 + s.vel[2]**2) < 9.0e-6:
        dx = 0.0
        dz = 0.0
    if abs(s.vel[1]) < 0.003:
        dy = 0.0
    s.vel = [dx, dy, dz]
    s.sprinting = sprint
    xxa, zza = modify_input(f32(strafe), f32(forward), sneak)
    if jump and s.on_ground and s.no_jump_delay == 0:
        jump_from_ground(world, s)
        s.no_jump_delay = 10
    elif not jump:
        s.no_jump_delay = 0
    if s.on_ground:
        fx, fy, fz = friction_block(s.pos)
        block_friction = compute_modified_friction(world.get_friction(fx, fy, fz), P.friction_modifier)
    else:
        block_friction = f32(1.0)
    # friction influenced speed
    if s.on_ground:
        if block_friction > 0.6:
            cubed = f32(f32(block_friction * block_friction) * block_friction)
            speed = f32(player_speed(s.sprinting) * f32(P.ground_accel / cubed))
        else:
            speed = player_speed(s.sprinting)
    else:
        speed = P.flying_speed
    ax, ay, az = input_vector(xxa, zza, speed, s.yaw)
    s.vel[0] += ax
    s.vel[1] += ay
    s.vel[2] += az
    climbing = world.is_climbable(math.floor(s.pos[0]), math.floor(s.pos[1]), math.floor(s.pos[2]))
    if climbing:
        s.vel = handle_on_climbable(s.vel, sneak)
    do_move(world, s, s.vel, sneak)
    mvx, mvy, mvz = s.vel
    if (s.hcol or jump) and climbing:
        mvy = 0.2
    if s.levitation is not None:
        movement_y = mvy + (0.05 * float(s.levitation + 1) - mvy) * 0.2
    else:
        falling = mvy <= 0.0
        movement_y = mvy - effective_gravity(float(P.gravity), falling, s.slow_falling)
    air_drag = compute_modified_friction(P.air_drag, P.air_drag_modifier)
    friction = f32(block_friction * air_drag)
    vfric = compute_modified_friction(P.vertical_air_drag, P.air_drag_modifier)
    s.vel = [mvx * float(friction), movement_y * float(vfric), mvz * float(friction)]


# ---- Scenarios --------------------------------------------------------------
def is_in_water(world, s):
    bb = bounding_box(s)
    min_x = mth_floor(bb[0] + 0.001)
    max_x = mth_floor(bb[3] - 0.001)
    min_y = mth_floor(bb[1] + 0.001)
    max_y = mth_floor(bb[4] - 0.001)
    min_z = mth_floor(bb[2] + 0.001)
    max_z = mth_floor(bb[5] - 0.001)
    for x in range(min_x, max_x + 1):
        for y in range(min_y, max_y + 1):
            for z in range(min_z, max_z + 1):
                if world.is_water(x, y, z):
                    return True
    return False


def effective_gravity(base_gravity, falling, slow_falling):
    if falling and slow_falling:
        return min(base_gravity, 0.01)
    return base_gravity


def fluid_falling_adjusted_movement(base_gravity, is_falling, sprinting, mv):
    if base_gravity != 0.0 and not sprinting:
        step = base_gravity / 16.0
        if is_falling and abs(mv[1] - 0.005) >= 0.003 and abs(mv[1] - step) < 0.003:
            yd = -0.003
        else:
            yd = mv[1] - step
        return (mv[0], yd, mv[2])
    return mv


def tick_water(world, s, forward, strafe, jump, sneak, sprint):
    apply_fluid_push(world, s, "water", 0.014)
    if s.no_jump_delay > 0:
        s.no_jump_delay -= 1
    dx, dy, dz = s.vel
    if (s.vel[0]**2 + s.vel[2]**2) < 9.0e-6:
        dx = 0.0
        dz = 0.0
    if abs(s.vel[1]) < 0.003:
        dy = 0.0
    s.vel = [dx, dy, dz]
    s.sprinting = sprint
    xxa, zza = modify_input(f32(strafe), f32(forward), sneak)
    if jump:
        s.vel[1] += float(f32(0.04))
    else:
        s.no_jump_delay = 0
    is_falling = s.vel[1] <= 0.0
    base_gravity = effective_gravity(float(P.gravity), is_falling, s.slow_falling)
    if s.dolphins_grace:
        slow_down = f32(0.96)
    elif s.sprinting:
        slow_down = P.water_sprint_slow_down
    else:
        slow_down = P.water_slow_down
    speed = P.fluid_input_speed
    ax, ay, az = input_vector(xxa, zza, speed, s.yaw)
    s.vel[0] += ax
    s.vel[1] += ay
    s.vel[2] += az
    do_move(world, s, s.vel, sneak)
    mv = (
        s.vel[0] * float(slow_down),
        s.vel[1] * float(f32(0.8)),
        s.vel[2] * float(slow_down),
    )
    s.vel = list(fluid_falling_adjusted_movement(base_gravity, is_falling, s.sprinting, mv))


def is_in_lava(world, s):
    bb = bounding_box(s)
    min_x = mth_floor(bb[0] + 0.001)
    max_x = mth_floor(bb[3] - 0.001)
    min_y = mth_floor(bb[1] + 0.001)
    max_y = mth_floor(bb[4] - 0.001)
    min_z = mth_floor(bb[2] + 0.001)
    max_z = mth_floor(bb[5] - 0.001)
    for x in range(min_x, max_x + 1):
        for y in range(min_y, max_y + 1):
            for z in range(min_z, max_z + 1):
                if world.is_lava(x, y, z):
                    return True
    return False


def tick_lava(world, s, forward, strafe, jump, sneak, sprint):
    apply_fluid_push(world, s, "lava", 0.0023333333333333335)
    if s.no_jump_delay > 0:
        s.no_jump_delay -= 1
    dx, dy, dz = s.vel
    if (s.vel[0]**2 + s.vel[2]**2) < 9.0e-6:
        dx = 0.0
        dz = 0.0
    if abs(s.vel[1]) < 0.003:
        dy = 0.0
    s.vel = [dx, dy, dz]
    s.sprinting = sprint
    xxa, zza = modify_input(f32(strafe), f32(forward), sneak)
    if jump:
        s.vel[1] += float(f32(0.04))
    else:
        s.no_jump_delay = 0
    base_gravity = float(P.gravity)
    ax, ay, az = input_vector(xxa, zza, P.fluid_input_speed, s.yaw)
    s.vel[0] += ax
    s.vel[1] += ay
    s.vel[2] += az
    do_move(world, s, s.vel, sneak)
    # deep-lava branch: scale(0.5) then -baseGravity/4
    s.vel = [s.vel[0] * 0.5, s.vel[1] * 0.5, s.vel[2] * 0.5]
    if base_gravity != 0.0:
        s.vel[1] += -base_gravity / 4.0


def calculate_view_vector(pitch, yaw):
    # Entity.calculateViewVector: LUT (float) trig, components rounded to float
    # then widened to double by the Vec3 constructor.
    real_x_rot = f32(f32(pitch) * f32(math.pi / 180.0))
    real_y_rot = f32(-f32(yaw) * f32(math.pi / 180.0))
    y_cos = mth_cos(real_y_rot)
    y_sin = mth_sin(real_y_rot)
    x_cos = mth_cos(real_x_rot)
    x_sin = mth_sin(real_x_rot)
    return [f32(y_sin * x_cos), f32(-x_sin), f32(y_cos * x_cos)]


def update_fall_flying_movement(s, mx, my, mz):
    # LivingEntity.updateFallFlyingMovement. Note the two trig sources: the look
    # vector uses Mth (LUT/float) while liftForce/lean use java.lang.Math (double).
    look = calculate_view_vector(s.pitch, s.yaw)
    lean_angle = f32(f32(s.pitch) * f32(math.pi / 180.0))
    look_hor_len = math.sqrt(look[0] * look[0] + look[2] * look[2])
    move_hor_len = math.sqrt(mx * mx + mz * mz)
    gravity = effective_gravity(float(P.gravity), my <= 0.0, s.slow_falling)
    cos_lean = math.cos(lean_angle)
    lift_force = cos_lean * cos_lean
    my = my + gravity * (-1.0 + lift_force * 0.75)
    if my < 0.0 and look_hor_len > 0.0:
        convert = my * -0.1 * lift_force
        mx = mx + look[0] * convert / look_hor_len
        my = my + convert
        mz = mz + look[2] * convert / look_hor_len
    if lean_angle < 0.0 and look_hor_len > 0.0:
        convert = move_hor_len * float(-mth_sin(lean_angle)) * 0.04
        mx = mx + -look[0] * convert / look_hor_len
        my = my + convert * 3.2
        mz = mz + -look[2] * convert / look_hor_len
    if look_hor_len > 0.0:
        mx = mx + (look[0] / look_hor_len * move_hor_len - mx) * 0.1
        mz = mz + (look[2] / look_hor_len * move_hor_len - mz) * 0.1
    return [mx * float(f32(0.99)), my * float(f32(0.98)), mz * float(f32(0.99))]


def tick_elytra(world, s):
    # travelFallFlying client path: aiStep velocity collapse, elytra update,
    # then move() with collision. WASD input is ignored during elytra flight.
    if s.no_jump_delay > 0:
        s.no_jump_delay -= 1
    dx, dy, dz = s.vel
    if s.vel[0] * s.vel[0] + s.vel[2] * s.vel[2] < 9.0e-6:
        dx = 0.0
        dz = 0.0
    if abs(s.vel[1]) < 0.003:
        dy = 0.0
    s.vel = update_fall_flying_movement(s, dx, dy, dz)
    do_move(world, s, list(s.vel), suppress_bounce=False)


def tick(world, s, forward, strafe, jump, sneak, sprint):
    if is_in_water(world, s):
        tick_water(world, s, forward, strafe, jump, sneak, sprint)
    elif is_in_lava(world, s):
        tick_lava(world, s, forward, strafe, jump, sneak, sprint)
    else:
        tick_air(world, s, forward, strafe, jump, sneak, sprint)


def flat_floor(y=0, r=4):
    w = World()
    for x in range(-r, r + 1):
        for z in range(-r, r + 1):
            w.add_solid(x, y, z)
    return w


def scenario_free_fall():
    w = World()
    s = State(0.5, 100.0, 0.5, 0.0)
    trace = []
    for _ in range(200):
        tick_air(w, s, 0.0, 0.0, False, False, False)
        trace.append((list(s.pos), list(s.vel)))
    return w, trace


def scenario_walk_flat():
    w = flat_floor()
    s = State(0.5, 1.0, 0.5, 0.0)
    s.on_ground = True
    trace = []
    for _ in range(200):
        tick_air(w, s, 1.0, 0.0, False, False, False)
        trace.append((list(s.pos), list(s.vel)))
    return w, trace


def scenario_sprint_jump():
    w = flat_floor()
    s = State(0.5, 1.0, 0.5, 0.0)
    s.on_ground = True
    trace = []
    for _ in range(120):
        tick_air(w, s, 1.0, 0.0, True, False, True)
        trace.append((list(s.pos), list(s.vel)))
    return w, trace


def scenario_ice_slide():
    # Ice floor (friction 0.98). Walk forward for 40 ticks, then release for 160.
    w = flat_floor()
    for x in range(-4, 5):
        for z in range(-4, 5):
            w.set_friction(x, 0, z, 0.98)
    s = State(0.5, 1.0, 0.5, 0.0)
    s.on_ground = True
    trace = []
    for t in range(200):
        fwd = 1.0 if t < 40 else 0.0
        tick_air(w, s, fwd, 0.0, False, False, False)
        trace.append((list(s.pos), list(s.vel)))
    return w, trace


def scenario_walk_into_wall():
    w = flat_floor()
    # Wall at x=1 spanning the player's path.
    for y in range(1, 3):
        for z in range(-2, 3):
            w.add_solid(1, y, z)
    s = State(0.5, 1.0, 0.5, 0.0)
    s.on_ground = True
    trace = []
    for _ in range(80):
        tick_air(w, s, 1.0, 0.0, False, False, False)
        trace.append((list(s.pos), list(s.vel)))
    return w, trace


def scenario_slab_step():
    w = flat_floor(r=6)
    # Slab (0.5 high) at x=1..3, on top of the floor (y=1).
    for x in (1, 2, 3):
        for z in range(-2, 3):
            w.add_box(x, 1, z, (0.0, 0.0, 0.0, 1.0, 0.5, 1.0))
    s = State(0.5, 1.0, 0.5, 0.0)
    s.on_ground = True
    trace = []
    for _ in range(60):
        tick_air(w, s, 1.0, 0.0, False, False, False)
        trace.append((list(s.pos), list(s.vel)))
    return w, trace


def scenario_water_sink():
    # A deep water column (x=0, z=0) with a solid floor far below. Player starts
    # fully submerged and sinks with no input, exercising travelInWater and the
    # -0.003 slow-sink clamp via getFluidFallingAdjustedMovement. Dispatched
    # through `tick` so the in-water branch is selected each tick.
    w = World()
    for y in range(80, 101):
        w.add_water(0, y, 0)
    w.add_solid(0, 78, 0)
    s = State(0.5, 95.0, 0.5, 0.0)
    trace = []
    for _ in range(120):
        if is_in_water(w, s):
            tick_water(w, s, 0.0, 0.0, False, False, False)
        else:
            tick_air(w, s, 0.0, 0.0, False, False, False)
        trace.append((list(s.pos), list(s.vel)))
    return w, trace


def scenario_diagonal_walk():
    # Strafe + forward held together, so modify_input's length uses the
    # two-term sqrt(sx*sx + sy*sy) that axis-aligned scenarios never exercise.
    w = flat_floor(r=8)
    s = State(0.5, 1.0, 0.5, 0.0)
    s.on_ground = True
    trace = []
    for _ in range(100):
        tick_air(w, s, 1.0, 1.0, False, False, False)
        trace.append((list(s.pos), list(s.vel)))
    return w, trace


def scenario_analog_strafe():
    # Asymmetric analog input (0.5 forward, 1.0 strafe) so sx != sy: the two-term
    # length rounds differently under per-op vs a single f64 cast. Confirms the
    # oracle (now per-op) and the Rust crate agree with the JVM here.
    w = flat_floor(r=8)
    s = State(0.5, 1.0, 0.5, 0.0)
    s.on_ground = True
    trace = []
    for _ in range(100):
        tick_air(w, s, 0.5, 1.0, False, False, False)
        trace.append((list(s.pos), list(s.vel)))
    return w, trace


def scenario_ladder_climb():
    # Vertical ladder column; player holds jump to climb steadily upward. Exercises
    # handle_on_climbable (pre-move clamp) and the post-move climb-up to 0.2.
    w = World()
    for y in range(0, 16):
        w.add_climbable(0, y, 0)
    s = State(0.5, 2.0, 0.5, 0.0)
    s.on_ground = False
    trace = []
    for _ in range(80):
        tick_air(w, s, 0.0, 0.0, True, False, False)
        trace.append((list(s.pos), list(s.vel)))
    return w, trace


def scenario_ladder_sneak_hold():
    # Sneaking on a ladder holds vertical position (yd forced to 0 while
    # descending) instead of sliding down.
    w = World()
    for y in range(0, 16):
        w.add_climbable(0, y, 0)
    s = State(0.5, 8.0, 0.5, 0.0)
    s.on_ground = False
    s.vel = [0.0, -0.2, 0.0]
    trace = []
    for _ in range(60):
        tick_air(w, s, 0.0, 0.0, False, True, False)
        trace.append((list(s.pos), list(s.vel)))
    return w, trace


def scenario_blue_ice_slide():
    # Blue ice is the slipperiest tier (friction 0.989F vs 0.98F for
    # ice/packed_ice). Same walk-then-release pattern; the higher friction means
    # a longer glide. Proves the data-driven friction path handles all tiers.
    w = flat_floor()
    for x in range(-4, 5):
        for z in range(-4, 5):
            w.set_friction(x, 0, z, 0.989)
    s = State(0.5, 1.0, 0.5, 0.0)
    s.on_ground = True
    trace = []
    for t in range(200):
        fwd = 1.0 if t < 40 else 0.0
        tick_air(w, s, fwd, 0.0, False, False, False)
        trace.append((list(s.pos), list(s.vel)))
    return w, trace


def scenario_lava_sink():
    # Deep lava column, no input. Exercises travelInLava's deep branch:
    # moveRelative(0.02) → move → scale(0.5) → -baseGravity/4. Different constants
    # AND a different branch from water (no 0.8/0.9 slow-down, no -0.003 clamp).
    w = World()
    for y in range(80, 101):
        w.add_lava(0, y, 0)
    w.add_solid(0, 78, 0)
    s = State(0.5, 95.0, 0.5, 0.0)
    trace = []
    for _ in range(120):
        tick(w, s, 0.0, 0.0, False, False, False)
        trace.append((list(s.pos), list(s.vel)))
    return w, trace


def scenario_levitation():
    # Levitation amplifier 0 in open air: gravity is *replaced* by a pull toward
    # 0.05*(amp+1)=0.05, so the player rises to a steady climb instead of falling.
    w = World()
    s = State(0.5, 100.0, 0.5, 0.0)
    s.levitation = 0
    trace = []
    for _ in range(120):
        tick(w, s, 0.0, 0.0, False, False, False)
        trace.append((list(s.pos), list(s.vel)))
    return w, trace


def scenario_slow_falling_water():
    # THE satisfying test: Slow Falling reduces getEffectiveGravity() to 0.01
    # while descending, so baseGravity/16 = 0.000625 != 0.005 and the otherwise
    # provably-dead -0.003 slow-sink clamp *fires*. Submerged, no input.
    w = World()
    for y in range(80, 101):
        w.add_water(0, y, 0)
    w.add_solid(0, 78, 0)
    s = State(0.5, 95.0, 0.5, 0.0)
    s.slow_falling = True
    trace = []
    for _ in range(120):
        tick(w, s, 0.0, 0.0, False, False, False)
        trace.append((list(s.pos), list(s.vel)))
    return w, trace


def scenario_swim_sprint():
    # Sprint-swimming with forward input: this is the swimming branch of
    # travelInWater (slowDown = 0.9F because sprinting), which the vertical-only
    # water_sink scenario never exercised. Confirms horizontal swim propulsion.
    w = World()
    for y in range(80, 101):
        for x in range(-2, 3):
            for z in range(-2, 3):
                w.add_water(x, y, z)
    s = State(0.5, 90.0, 0.5, 0.0)
    trace = []
    for _ in range(120):
        tick(w, s, 1.0, 0.0, False, False, True)
        trace.append((list(s.pos), list(s.vel)))
    return w, trace


def scenario_soul_sand_walk():
    # Synthetic soul-sand-like floor: a full collision cube carrying a block
    # speed factor of 0.4. The player rests at y=1.0, so blockPosition() (0,1,0)
    # is air (1.0) and getBlockSpeedFactor falls through to the block below
    # (0,0,0)=0.4 -- exercising the here==1.0 fallback branch that no other
    # scenario hits, plus the multiply(f,1,f) applied at the end of move().
    w = flat_floor(r=8)
    for x in range(-8, 9):
        for z in range(-8, 9):
            w.set_speed_factor(x, 0, z, 0.4)
    s = State(0.5, 1.0, 0.5, 0.0)
    s.on_ground = True
    trace = []
    for _ in range(120):
        tick(w, s, 1.0, 0.0, False, False, False)
        trace.append((list(s.pos), list(s.vel)))
    return w, trace


def scenario_jump_boost():
    # Jump Boost II (amplifier 1): getJumpBoostPower() = 0.1F*(1+1) = 0.2F added
    # to the jump velocity as a separate float term. Repeated auto-jumps.
    w = flat_floor()
    s = State(0.5, 1.0, 0.5, 0.0)
    s.on_ground = True
    s.jump_boost = 1
    trace = []
    for _ in range(120):
        tick(w, s, 0.0, 0.0, True, False, False)
        trace.append((list(s.pos), list(s.vel)))
    return w, trace


def scenario_honey_jump():
    # Synthetic honey-like floor: full collision cube with jump factor 0.5.
    # getBlockJumpFactor() returns 0.5 (feet block is air=1.0 -> falls to the
    # block below), scaling jump velocity to 0.42*0.5 = 0.21F.
    w = flat_floor()
    for x in range(-4, 5):
        for z in range(-4, 5):
            w.set_jump_factor(x, 0, z, 0.5)
    s = State(0.5, 1.0, 0.5, 0.0)
    s.on_ground = True
    trace = []
    for _ in range(120):
        tick(w, s, 0.0, 0.0, True, False, False)
        trace.append((list(s.pos), list(s.vel)))
    return w, trace


def scenario_slime_bounce():
    # Slime floor (bounce_restitution 1.0). Player free-falls from y=6 onto it
    # and bounces: restituteMovementAfterCollisions reverses vy through the
    # block-bounciness branch. Long enough to see several decaying bounces.
    w = flat_floor()
    for x in range(-4, 5):
        for z in range(-4, 5):
            w.add_slime(x, 0, z)
    s = State(0.5, 6.0, 0.5, 0.0)
    trace = []
    for _ in range(160):
        tick(w, s, 0.0, 0.0, False, False, False)
        trace.append((list(s.pos), list(s.vel)))
    return w, trace


def scenario_slime_bounce_sneak():
    # Same slime floor, but the player holds sneak on landing. isSuppressingBounce
    # both zeroes the base restitution and vetoes the block-bounce branch, so the
    # player lands and rests (vy -> 0) instead of bouncing.
    w = flat_floor()
    for x in range(-4, 5):
        for z in range(-4, 5):
            w.add_slime(x, 0, z)
    s = State(0.5, 6.0, 0.5, 0.0)
    trace = []
    for _ in range(160):
        tick(w, s, 0.0, 0.0, False, True, False)
        trace.append((list(s.pos), list(s.vel)))
    return w, trace


def scenario_elytra_glide_level():
    # Elytra, pitch 0 / yaw 0: level launch with forward speed. liftForce = 1,
    # so the vertical term is gravity*(-1 + 0.75). Pure +Z glide, gentle sink.
    w = World()
    s = State(0.5, 100.0, 0.5, 0.0)
    s.pitch = f32(0.0)
    s.fall_flying = True
    s.vel = [0.0, 0.0, 1.0]
    trace = []
    for _ in range(160):
        tick_elytra(w, s)
        trace.append((list(s.pos), list(s.vel)))
    return w, trace


def scenario_elytra_dive():
    # Elytra, pitch +37 deg (nose-down, off-grid angle). my goes negative fast,
    # firing the `my < 0` convert branch that trades altitude for speed. The
    # non-nice pitch forces the LUT-cos vs Math.cos difference to be observable.
    w = World()
    s = State(0.5, 200.0, 0.5, 0.0)
    s.pitch = f32(37.0)
    s.fall_flying = True
    s.vel = [0.0, 0.0, 0.8]
    trace = []
    for _ in range(160):
        tick_elytra(w, s)
        trace.append((list(s.pos), list(s.vel)))
    return w, trace


def scenario_elytra_climb():
    # Elytra, pitch -23 deg (nose-up). Exercises the `leanAngle < 0` branch:
    # -Mth.sin(leanAngle) > 0, adding convert*3.2 lift plus a backward
    # horizontal component. The pump-up-then-stall arc.
    w = World()
    s = State(0.5, 100.0, 0.5, 0.0)
    s.pitch = f32(-23.0)
    s.fall_flying = True
    s.vel = [0.0, 0.0, 1.4]
    trace = []
    for _ in range(160):
        tick_elytra(w, s)
        trace.append((list(s.pos), list(s.vel)))
    return w, trace


def scenario_elytra_diagonal_yaw():
    # Elytra, yaw 33 deg / pitch +11 deg: look.x and look.z both nonzero and
    # unequal, so the `look/lookHorLength * moveHorLength - m` steer redistributes
    # speed onto both axes with different per-axis rounding -- an asymmetric case
    # a pure +Z glide cannot reach.
    w = World()
    s = State(0.5, 150.0, 0.5, 33.0)
    s.pitch = f32(11.0)
    s.fall_flying = True
    s.vel = [0.3, 0.0, 0.9]
    trace = []
    for _ in range(160):
        tick_elytra(w, s)
        trace.append((list(s.pos), list(s.vel)))
    return w, trace


def scenario_water_current_push():
    # Sustained fluid-current push. The player is submerged in a horizontal water
    # gradient: source columns (amount 8) at x<=0 and shallower flowing water
    # (amount 5) at x>0 create a steady eastward current via FlowingFluid.getFlow.
    # Exercises the full push path (box scan, neighbour heights, player 1/count
    # averaging, the 0.014 scale, the 0.0045 minimum-impulse floor when at rest)
    # accumulating against water drag over 100 ticks. Dispatched through `tick`,
    # so the in-water branch and the baseTick push run each tick. Sustained so a
    # counter-style regression in the accumulation would diverge visibly.
    w = World()
    for x in range(-2, 12):
        for z in range(-1, 2):
            w.add_solid(x, 0, z)
            for y in (1, 2):
                w.add_water_cell(x, y, z, 8 if x <= 0 else 5)
    s = State(0.5, 1.0, 0.5, 0.0)
    s.on_ground = True
    trace = []
    for _ in range(100):
        tick(w, s, 0.0, 0.0, False, False, False)
        trace.append((list(s.pos), list(s.vel)))
    return w, trace


SCENARIOS = [
    ("free_fall", scenario_free_fall),
    ("walk_flat", scenario_walk_flat),
    ("sprint_jump", scenario_sprint_jump),
    ("ice_slide", scenario_ice_slide),
    ("walk_into_wall", scenario_walk_into_wall),
    ("slab_step", scenario_slab_step),
    ("water_sink", scenario_water_sink),
    ("diagonal_walk", scenario_diagonal_walk),
    ("analog_strafe", scenario_analog_strafe),
    ("ladder_climb", scenario_ladder_climb),
    ("ladder_sneak_hold", scenario_ladder_sneak_hold),
    ("blue_ice_slide", scenario_blue_ice_slide),
    ("lava_sink", scenario_lava_sink),
    ("levitation", scenario_levitation),
    ("slow_falling_water", scenario_slow_falling_water),
    ("swim_sprint", scenario_swim_sprint),
    ("soul_sand_walk", scenario_soul_sand_walk),
    ("jump_boost", scenario_jump_boost),
    ("honey_jump", scenario_honey_jump),
    ("slime_bounce", scenario_slime_bounce),
    ("slime_bounce_sneak", scenario_slime_bounce_sneak),
    ("elytra_glide_level", scenario_elytra_glide_level),
    ("elytra_dive", scenario_elytra_dive),
    ("elytra_climb", scenario_elytra_climb),
    ("elytra_diagonal_yaw", scenario_elytra_diagonal_yaw),
    ("water_current_push", scenario_water_current_push),
]


def bits(x):
    return struct.unpack("<Q", struct.pack("<d", float(x)))[0]


def main():
    out = []
    out.append("//! AUTO-GENERATED golden movement traces. DO NOT EDIT.")
    out.append("//!")
    out.append("//! Produced by `gen_golden.py`, an independent Python oracle (see that")
    out.append("//! file's header). Each value is an `f64` bit pattern so the Rust test")
    out.append("//! asserts bit-for-bit equality with no decimal-parsing rounding.")
    out.append("")
    out.append("/// One tick of ground truth: position then velocity, as raw `f64` bits.")
    out.append("#[derive(Debug)]")
    out.append("pub struct GoldenTick {")
    out.append("    /// Position `(x, y, z)` bits.")
    out.append("    pub pos: [u64; 3],")
    out.append("    /// Velocity `(x, y, z)` bits.")
    out.append("    pub vel: [u64; 3],")
    out.append("}")
    out.append("")
    for name, fn in SCENARIOS:
        _, trace = fn()
        const = "GOLDEN_" + name.upper()
        out.append(f"/// Golden trace for the `{name}` scenario ({len(trace)} ticks).")
        out.append(f"pub static {const}: [GoldenTick; {len(trace)}] = [")
        for pos, vel in trace:
            pb = ", ".join(f"0x{bits(v):016x}" for v in pos)
            vb = ", ".join(f"0x{bits(v):016x}" for v in vel)
            out.append(f"    GoldenTick {{ pos: [{pb}], vel: [{vb}] }},")
        out.append("];")
        out.append("")
    with open("crates/lodestone-physics/tests/support/golden_traces.rs", "w") as fh:
        fh.write("\n".join(out) + "\n")
    # Print the free-fall anchor for human verification.
    _, ff = scenario_free_fall()
    print("free_fall first 4 dy:", [round(ff[i][0][1] - (ff[i-1][0][1] if i else 100.0), 8) for i in range(4)])


if __name__ == "__main__":
    main()
