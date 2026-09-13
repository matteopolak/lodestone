// Independent Java movement oracle for the Lodestone physics crate.
//
// This is NOT decompiled/copied Mojang code. It is a from-scratch Java
// re-implementation of the *documented* movement algorithm (constants + the
// order of operations described in the report), written to obtain ground-truth
// position/velocity bit patterns from the real JVM — the language whose
// `float`/`double` semantics we claim to reproduce in Rust. Crucially it uses
// native `float` where vanilla uses `float` and `double` where vanilla uses
// `double`, so per-operation rounding matches the game rather than being
// simulated (as the Python oracle does).
//
// It prints, per scenario, one line `SCENARIO <name> <ticks>` followed by one
// line per tick: six unsigned decimals = raw Double.doubleToRawLongBits of
// px py pz vx vy vz. A comparator diffs these against the checked-in traces.

import java.util.ArrayList;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;

public final class MoveOracle {
    // ---- Mth --------------------------------------------------------------
    static final double SIN_SCALE = 10430.378350470453;
    static final float[] SIN = new float[65536];
    static {
        for (int i = 0; i < 65536; i++) {
            SIN[i] = (float) Math.sin(i / SIN_SCALE);
        }
    }

    static float sin(double i) {
        return SIN[(int) ((long) (i * SIN_SCALE) & 65535L)];
    }

    static float cos(double i) {
        return SIN[(int) ((long) (i * SIN_SCALE + 16384.0) & 65535L)];
    }

    static int floor(double v) {
        int i = (int) v;
        return v < i ? i - 1 : i;
    }

    static float clampF(float v, float lo, float hi) {
        return v < lo ? lo : (v > hi ? hi : v);
    }

    // Mth.clamp(double, double, double): value < min ? min : Math.min(value, max)
    static double clampDouble(double v, double lo, double hi) {
        return v < lo ? lo : Math.min(v, hi);
    }

    static float computeModifiedFriction(float friction, float modifier) {
        return clampF(1.0F - (1.0F - friction) * modifier, 0.0F, 1.0F);
    }

    // ---- Profile (all widths mirror vanilla) ------------------------------
    static final float BASE_MOVEMENT_SPEED = 0.1F;
    static final float SPRINT_SPEED_MODIFIER = 0.3F;
    static final float SNEAKING_SPEED = 0.3F;
    static final float GRAVITY = 0.08F;
    static final float AIR_DRAG = 0.91F;
    static final float VERTICAL_AIR_DRAG = 0.98F;
    static final float AIR_DRAG_MODIFIER = 1.0F;
    static final float FRICTION_MODIFIER = 1.0F;
    static final float GROUND_ACCEL = 0.21600002F;
    static final float FLYING_SPEED = 0.02F;
    static final float JUMP_POWER = 0.42F;
    static final double SPRINT_JUMP_BOOST = 0.2; // double literal, per source
    static final float STEP_HEIGHT = 0.6F;
    static final float WIDTH = 0.6F;
    static final float HEIGHT = 1.8F;
    static final float WATER_SLOW_DOWN = 0.8F;
    static final float WATER_SPRINT_SLOW_DOWN = 0.9F;
    static final float FLUID_INPUT_SPEED = 0.02F;

    static final double EPS = 1.0e-7;

    // ---- World ------------------------------------------------------------
    static final class World {
        final Set<Long> solid = new HashSet<>();
        final Map<Long, List<double[]>> boxes = new HashMap<>();
        final Map<Long, Float> friction = new HashMap<>();
        final Set<Long> water = new HashSet<>();
        final Set<Long> climbable = new HashSet<>();
        final Set<Long> lava = new HashSet<>();
        final Set<Long> slime = new HashSet<>();
        final Map<Long, Float> jumpFactor = new HashMap<>();
        final Map<Long, Float> speedFactor = new HashMap<>();

        static long key(int x, int y, int z) {
            return ((long) x & 0x1fffff) | (((long) y & 0x1fffff) << 21) | (((long) z & 0x1fffff) << 42);
        }

        void addSolid(int x, int y, int z) {
            solid.add(key(x, y, z));
        }

        void addBox(int x, int y, int z, double[] local) {
            double[] w = {
                local[0] + x, local[1] + y, local[2] + z,
                local[3] + x, local[4] + y, local[5] + z,
            };
            boxes.computeIfAbsent(key(x, y, z), k -> new ArrayList<>()).add(w);
        }

        void setFriction(int x, int y, int z, float f) {
            friction.put(key(x, y, z), f);
        }

        void addWater(int x, int y, int z) {
            water.add(key(x, y, z));
        }

        void collisionBoxes(int x, int y, int z, List<double[]> out) {
            long k = key(x, y, z);
            if (boxes.containsKey(k)) {
                out.addAll(boxes.get(k));
            } else if (solid.contains(k)) {
                out.add(new double[] {x, y, z, x + 1.0, y + 1.0, z + 1.0});
            }
        }

        float getFriction(int x, int y, int z) {
            return friction.getOrDefault(key(x, y, z), 0.6F);
        }

        boolean isWater(int x, int y, int z) {
            return water.contains(key(x, y, z));
        }

        void addClimbable(int x, int y, int z) {
            climbable.add(key(x, y, z));
        }

        boolean isClimbable(int x, int y, int z) {
            return climbable.contains(key(x, y, z));
        }

        void addLava(int x, int y, int z) {
            lava.add(key(x, y, z));
        }

        boolean isLava(int x, int y, int z) {
            return lava.contains(key(x, y, z));
        }

        void addSlime(int x, int y, int z) {
            slime.add(key(x, y, z));
        }

        // Slime bounceRestitution 1.0F; a LivingEntity (player) does not get the
        // non-living *0.8 factor.
        float bounceRestitution(int x, int y, int z) {
            return slime.contains(key(x, y, z)) ? 1.0F : 0.0F;
        }

        void setJumpFactor(int x, int y, int z, float f) {
            jumpFactor.put(key(x, y, z), f);
        }

        float getJumpFactor(int x, int y, int z) {
            return jumpFactor.getOrDefault(key(x, y, z), 1.0F);
        }

        void setSpeedFactor(int x, int y, int z, float f) {
            speedFactor.put(key(x, y, z), f);
        }

        float getSpeedFactor(int x, int y, int z) {
            return speedFactor.getOrDefault(key(x, y, z), 1.0F);
        }
    }

    // ---- Collision --------------------------------------------------------
    static List<double[]> gather(World w, double[] r) {
        int minx = floor(r[0]) - 1, miny = floor(r[1]) - 1, minz = floor(r[2]) - 1;
        int maxx = floor(r[3]) + 1, maxy = floor(r[4]) + 1, maxz = floor(r[5]) + 1;
        List<double[]> out = new ArrayList<>();
        for (int x = minx; x <= maxx; x++) {
            for (int y = miny; y <= maxy; y++) {
                for (int z = minz; z <= maxz; z++) {
                    w.collisionBoxes(x, y, z, out);
                }
            }
        }
        return out;
    }

    static double[] expandTowards(double[] bb, double xa, double ya, double za) {
        double[] r = bb.clone();
        if (xa < 0.0) r[0] += xa;
        else if (xa > 0.0) r[3] += xa;
        if (ya < 0.0) r[1] += ya;
        else if (ya > 0.0) r[4] += ya;
        if (za < 0.0) r[2] += za;
        else if (za > 0.0) r[5] += za;
        return r;
    }

    static double bbMin(double[] bb, int axis) {
        return bb[axis];
    }

    static double bbMax(double[] bb, int axis) {
        return bb[axis + 3];
    }

    static double collideOne(int axis, double[] shape, double[] moving, double distance) {
        int b = -1, c = -1;
        for (int ax = 0; ax < 3; ax++) {
            if (ax == axis) continue;
            if (b < 0) b = ax;
            else c = ax;
        }
        if (!(bbMax(moving, b) - EPS > bbMin(shape, b) && bbMin(moving, b) + EPS < bbMax(shape, b))) {
            return distance;
        }
        if (!(bbMax(moving, c) - EPS > bbMin(shape, c) && bbMin(moving, c) + EPS < bbMax(shape, c))) {
            return distance;
        }
        double maxa = bbMax(moving, axis);
        double mina = bbMin(moving, axis);
        if (distance > 0.0) {
            if (maxa - EPS <= bbMin(shape, axis)) {
                double nd = bbMin(shape, axis) - maxa;
                if (nd >= -EPS) return Math.min(distance, nd);
            }
        } else if (distance < 0.0) {
            if (mina + EPS >= bbMax(shape, axis)) {
                double nd = bbMax(shape, axis) - mina;
                if (nd <= EPS) return Math.max(distance, nd);
            }
        }
        return distance;
    }

    static double collideAxis(int axis, double[] moving, List<double[]> shapes, double distance) {
        for (double[] shape : shapes) {
            if (Math.abs(distance) < EPS) return 0.0;
            distance = collideOne(axis, shape, moving, distance);
        }
        return distance;
    }

    static double[] bbMove(double[] bb, double dx, double dy, double dz) {
        return new double[] {bb[0] + dx, bb[1] + dy, bb[2] + dz, bb[3] + dx, bb[4] + dy, bb[5] + dz};
    }

    static int[] axisStepOrder(double mx, double mz) {
        return Math.abs(mx) < Math.abs(mz) ? new int[] {1, 2, 0} : new int[] {1, 0, 2};
    }

    static double[] collideWithShapes(double[] mv, double[] bb, List<double[]> shapes) {
        if (shapes.isEmpty()) return mv.clone();
        double[] resolved = {0.0, 0.0, 0.0};
        for (int axis : axisStepOrder(mv[0], mv[2])) {
            double am = mv[axis];
            if (am != 0.0) {
                double[] moved = bbMove(bb, resolved[0], resolved[1], resolved[2]);
                resolved[axis] = collideAxis(axis, moved, shapes, am);
            }
        }
        return resolved;
    }

    static List<Float> candidateStepHeights(double[] bb, List<double[]> colliders, float maxStep, float skip) {
        List<Float> cands = new ArrayList<>();
        for (double[] col : colliders) {
            boolean broke = false;
            for (double coord : new double[] {col[1], col[4]}) {
                float rel = (float) (coord - bb[1]);
                if (rel >= 0.0F && rel != skip) {
                    if (rel > maxStep) {
                        broke = true;
                        break;
                    }
                    if (!cands.contains(rel)) cands.add(rel);
                }
            }
            if (broke) continue;
        }
        cands.sort(Float::compare);
        return cands;
    }

    static double hdistSqr(double[] v) {
        return v[0] * v[0] + v[2] * v[2];
    }

    static double[] collide(World world, double[] mv, double[] bb, boolean onGround, float maxStep) {
        double lensq = mv[0] * mv[0] + mv[1] * mv[1] + mv[2] * mv[2];
        double[] step;
        if (lensq == 0.0) {
            step = mv.clone();
        } else {
            double[] region = expandTowards(bb, mv[0], mv[1], mv[2]);
            List<double[]> shapes = gather(world, region);
            step = collideWithShapes(mv, bb, shapes);
        }
        boolean xcol = mv[0] != step[0];
        boolean ycol = mv[1] != step[1];
        boolean zcol = mv[2] != step[2];
        boolean onGroundAfter = ycol && mv[1] < 0.0;
        if (maxStep > 0.0F && (onGroundAfter || onGround) && (xcol || zcol)) {
            double[] grounded = onGroundAfter ? bbMove(bb, 0.0, step[1], 0.0) : bb;
            double[] stepBox = expandTowards(grounded, mv[0], maxStep, mv[2]);
            if (!onGroundAfter) stepBox = expandTowards(stepBox, 0.0, -EPS, 0.0);
            List<double[]> colliders = gather(world, stepBox);
            float skip = (float) step[1];
            for (float cand : candidateStepHeights(grounded, colliders, maxStep, skip)) {
                double[] s = collideWithShapes(new double[] {mv[0], cand, mv[2]}, grounded, colliders);
                if (hdistSqr(s) > hdistSqr(step)) {
                    double dtg = bb[1] - grounded[1];
                    return new double[] {s[0], s[1] - dtg, s[2]};
                }
            }
        }
        return step;
    }

    // ---- Player -----------------------------------------------------------
    static final class State {
        double px, py, pz;
        double vx, vy, vz;
        float yaw;
        float pitch;
        boolean fallFlying;
        boolean onGround;
        boolean hcol;
        int noJumpDelay;
        boolean sprinting;
        // Physics-affecting status effects.
        Integer levitation = null; // null or amplifier (0-based)
        boolean slowFalling = false;
        boolean dolphinsGrace = false;
        Integer jumpBoost = null; // null or amplifier (0-based)

        State(double x, double y, double z, float yaw) {
            this.px = x;
            this.py = y;
            this.pz = z;
            this.yaw = yaw;
        }
    }

    static double[] boundingBox(State s) {
        double half = WIDTH / 2.0;
        return new double[] {
            s.px - half, s.py, s.pz - half,
            s.px + half, s.py + HEIGHT, s.pz + half,
        };
    }

    static float playerSpeed(boolean sprinting) {
        double base = BASE_MOVEMENT_SPEED;
        if (sprinting) {
            return (float) (base * (1.0 + (double) SPRINT_SPEED_MODIFIER));
        }
        return (float) base;
    }

    // Returns {xxa, zza}. Uses native float per-operation rounding.
    static float[] modifyInput(float strafe, float forward, boolean sneak) {
        if (strafe * strafe + forward * forward == 0.0F) {
            return new float[] {strafe, forward};
        }
        float sx = strafe * 0.98F;
        float sy = forward * 0.98F;
        if (sneak) {
            sx *= SNEAKING_SPEED;
            sy *= SNEAKING_SPEED;
        }
        float length = (float) Math.sqrt(sx * sx + sy * sy);
        if (length <= 0.0F) {
            return new float[] {sx, sy};
        }
        float dirX = sx / length;
        float dirY = sy / length;
        float ax = Math.abs(dirX);
        float ay = Math.abs(dirY);
        float tan = ay > ax ? ax / ay : ay / ax;
        float distToUnitSquare = (float) Math.sqrt(1.0F + tan * tan);
        float modifiedLength = Math.min(length * distToUnitSquare, 1.0F);
        return new float[] {dirX * modifiedLength, dirY * modifiedLength};
    }

    static double[] inputVector(float strafe, float forward, float speed, float yaw) {
        double ix = strafe, iz = forward;
        double lensq = ix * ix + iz * iz;
        if (lensq < 1.0e-7) return new double[] {0.0, 0.0, 0.0};
        if (lensq > 1.0) {
            double d = Math.sqrt(lensq);
            if (d < (double) 1.0e-5F) {
                ix = 0.0;
                iz = 0.0;
            } else {
                ix = ix / d;
                iz = iz / d;
            }
        }
        double mx = ix * speed;
        double mz = iz * speed;
        float rad = yaw * ((float) Math.PI / 180.0F);
        double s = sin(rad);
        double c = cos(rad);
        return new double[] {mx * c - mz * s, 0.0, mz * c + mx * s};
    }

    static int[] frictionBlock(double x, double y, double z) {
        return new int[] {floor(x), floor(y - (double) 0.500001F), floor(z)};
    }

    static void doMove(World world, State s, double dx, double dy, double dz, boolean suppressBounce) {
        double[] bb = boundingBox(s);
        double[] resolved = collide(world, new double[] {dx, dy, dz}, bb, s.onGround, STEP_HEIGHT);
        boolean xcol = Math.abs(dx - resolved[0]) >= (double) 1.0e-5F;
        boolean zcol = Math.abs(dz - resolved[2]) >= (double) 1.0e-5F;
        s.hcol = xcol || zcol;
        boolean movedVert = Math.abs(dy) > 0.0;
        boolean vcol = dy != resolved[1];
        boolean vbelow = vcol && dy < 0.0;
        s.onGround = vbelow;
        double lensqR = resolved[0] * resolved[0] + resolved[1] * resolved[1] + resolved[2] * resolved[2];
        double lensqD = dx * dx + dy * dy + dz * dz;
        if (lensqR > 1.0e-7 || lensqD - lensqR < 1.0e-7) {
            s.px += resolved[0];
            s.py += resolved[1];
            s.pz += resolved[2];
        }
        // deltaMovement stays == delta through collide; restitute rewrites it
        // (zeroing a blocked axis, or reversing it into a bounce off slime).
        if ((movedVert && vcol) || s.hcol) {
            double[] v = restitute(world, s, dx, dy, dz, resolved, xcol, zcol, vcol, vbelow, suppressBounce);
            s.vx = v[0];
            s.vy = v[1];
            s.vz = v[2];
        } else {
            s.vx = dx;
            s.vy = dy;
            s.vz = dz;
        }
        // Block speed factor (soul sand / honey), applied last as in move():
        // blockPosition() (floor of feet) first, then the block below if 1.0.
        int bx = floor(s.px), by = floor(s.py), bz = floor(s.pz);
        float sfHere = world.getSpeedFactor(bx, by, bz);
        float bsf;
        if (sfHere == 1.0F) {
            int[] fb = frictionBlock(s.px, s.py, s.pz);
            bsf = world.getSpeedFactor(fb[0], fb[1], fb[2]);
        } else {
            bsf = sfHere;
        }
        s.vx = s.vx * (double) bsf;
        s.vy = s.vy * 1.0;
        s.vz = s.vz * (double) bsf;
    }

    // Entity.restituteMovementAfterCollisions.
    static double[] restitute(World world, State s, double dx, double dy, double dz,
            double[] resolved, boolean xcol, boolean zcol, boolean vcol, boolean vbelow,
            boolean suppressBounce) {
        double restitution = 0.0; // getEntityBounciness() == 0.0 for a player
        double vx = dx, vy = dy, vz = dz;
        if (xcol) vx = -dx * restitution;
        if (zcol) vz = -dz * restitution;
        if (vcol) {
            if (vbelow) {
                int ex = floor(s.px);
                int ey = floor(s.py - (double) 0.2F);
                int ez = floor(s.pz);
                double blockB = world.bounceRestitution(ex, ey, ez);
                double eg = effectiveGravity((double) GRAVITY, dy <= 0.0, s.slowFalling);
                if (!(-dy < eg) && !suppressBounce) {
                    restitution = Math.max(restitution, blockB);
                } else {
                    restitution = 0.0;
                }
            }
            double gravComp;
            double effDrag;
            if (restitution > 0.0) {
                double portion = resolved[1] / dy;
                double eg = effectiveGravity((double) GRAVITY, dy <= 0.0, s.slowFalling);
                gravComp = portion * eg;
                effDrag = 1.0 + portion * ((double) VERTICAL_AIR_DRAG - 1.0); // lerp(portion,1,0.98)
            } else {
                gravComp = 0.0;
                effDrag = 1.0;
            }
            vy = (gravComp - dy) * effDrag * restitution;
        }
        return new double[] {vx, vy, vz};
    }

    static void jumpFromGround(World world, State s) {
        // getJumpPower(): JUMP_STRENGTH * multiplier(1) * getBlockJumpFactor() + getJumpBoostPower().
        float bjf = blockJumpFactor(world, s.px, s.py, s.pz);
        float jp = JUMP_POWER * bjf + jumpBoostPower(s.jumpBoost);
        if (jp <= 1.0e-5) return;
        s.vy = Math.max((double) jp, s.vy);
        if (s.sprinting) {
            float angle = s.yaw * ((float) Math.PI / 180.0F);
            s.vx += (double) (-sin(angle)) * SPRINT_JUMP_BOOST;
            s.vz += (double) (cos(angle)) * SPRINT_JUMP_BOOST;
        }
    }

    static float jumpBoostPower(Integer jumpBoost) {
        return jumpBoost == null ? 0.0F : 0.1F * (jumpBoost + 1.0F);
    }

    static float blockJumpFactor(World world, double px, double py, double pz) {
        float here = world.getJumpFactor(floor(px), floor(py), floor(pz));
        if (here == 1.0F) {
            int[] fb = frictionBlock(px, py, pz);
            return world.getJumpFactor(fb[0], fb[1], fb[2]);
        }
        return here;
    }

    static float frictionInfluencedSpeed(State s, float blockFriction) {
        if (s.onGround) {
            if (blockFriction > 0.6F) {
                float cubed = blockFriction * blockFriction * blockFriction;
                return playerSpeed(s.sprinting) * (GROUND_ACCEL / cubed);
            }
            return playerSpeed(s.sprinting);
        }
        return FLYING_SPEED;
    }

    static void tickAir(World world, State s, float forward, float strafe, boolean jump, boolean sneak, boolean sprint) {
        if (s.noJumpDelay > 0) s.noJumpDelay -= 1;
        double dx = s.vx, dy = s.vy, dz = s.vz;
        if (s.vx * s.vx + s.vz * s.vz < 9.0e-6) {
            dx = 0.0;
            dz = 0.0;
        }
        if (Math.abs(s.vy) < 0.003) dy = 0.0;
        s.vx = dx;
        s.vy = dy;
        s.vz = dz;
        s.sprinting = sprint;
        float[] in = modifyInput(strafe, forward, sneak);
        float xxa = in[0], zza = in[1];
        if (jump && s.onGround && s.noJumpDelay == 0) {
            jumpFromGround(world, s);
            s.noJumpDelay = 10;
        } else if (!jump) {
            s.noJumpDelay = 0;
        }
        float blockFriction;
        if (s.onGround) {
            int[] fb = frictionBlock(s.px, s.py, s.pz);
            blockFriction = computeModifiedFriction(world.getFriction(fb[0], fb[1], fb[2]), FRICTION_MODIFIER);
        } else {
            blockFriction = 1.0F;
        }
        float speed = frictionInfluencedSpeed(s, blockFriction);
        double[] accel = inputVector(xxa, zza, speed, s.yaw);
        s.vx += accel[0];
        s.vy += accel[1];
        s.vz += accel[2];
        boolean climbing = world.isClimbable(floor(s.px), floor(s.py), floor(s.pz));
        if (climbing) {
            double bound = (double) 0.15F;
            s.vx = clampDouble(s.vx, -bound, bound);
            s.vz = clampDouble(s.vz, -bound, bound);
            double yd = Math.max(s.vy, -bound);
            if (yd < 0.0 && sneak) yd = 0.0;
            s.vy = yd;
        }
        doMove(world, s, s.vx, s.vy, s.vz, sneak);
        double mvx = s.vx, mvy = s.vy, mvz = s.vz;
        if ((s.hcol || jump) && climbing) {
            mvy = 0.2;
        }
        double movementY;
        if (s.levitation != null) {
            movementY = mvy + (0.05 * (double) (s.levitation + 1) - mvy) * 0.2;
        } else {
            boolean falling = mvy <= 0.0;
            movementY = mvy - effectiveGravity((double) GRAVITY, falling, s.slowFalling);
        }
        float airDrag = computeModifiedFriction(AIR_DRAG, AIR_DRAG_MODIFIER);
        float friction = blockFriction * airDrag;
        float vfric = computeModifiedFriction(VERTICAL_AIR_DRAG, AIR_DRAG_MODIFIER);
        s.vx = mvx * (double) friction;
        s.vy = movementY * (double) vfric;
        s.vz = mvz * (double) friction;
    }

    // Entity.calculateViewVector(xRot, yRot): the LUT-based (float) look vector,
    // widened to double components in the Vec3 constructor.
    static double[] calculateViewVector(float xRot, float yRot) {
        float realXRot = xRot * (float) (Math.PI / 180.0);
        float realYRot = -yRot * (float) (Math.PI / 180.0);
        float yCos = cos(realYRot);
        float ySin = sin(realYRot);
        float xCos = cos(realXRot);
        float xSin = sin(realXRot);
        return new double[] {ySin * xCos, -xSin, yCos * xCos};
    }

    static double mthSquare(double v) {
        return v * v;
    }

    // LivingEntity.updateFallFlyingMovement. Returns the new deltaMovement.
    static double[] updateFallFlyingMovement(State s, double mx, double my, double mz) {
        double[] look = calculateViewVector(s.pitch, s.yaw);
        float leanAngle = s.pitch * (float) (Math.PI / 180.0);
        double lookHorLength = Math.sqrt(look[0] * look[0] + look[2] * look[2]);
        double moveHorLength = Math.sqrt(mx * mx + mz * mz);
        double gravity = effectiveGravity((double) GRAVITY, my <= 0.0, s.slowFalling);
        double liftForce = mthSquare(Math.cos(leanAngle));
        my = my + gravity * (-1.0 + liftForce * 0.75);
        if (my < 0.0 && lookHorLength > 0.0) {
            double convert = my * -0.1 * liftForce;
            mx = mx + look[0] * convert / lookHorLength;
            my = my + convert;
            mz = mz + look[2] * convert / lookHorLength;
        }
        if (leanAngle < 0.0F && lookHorLength > 0.0) {
            double convert = moveHorLength * (double) (-sin(leanAngle)) * 0.04;
            mx = mx + -look[0] * convert / lookHorLength;
            my = my + convert * 3.2;
            mz = mz + -look[2] * convert / lookHorLength;
        }
        if (lookHorLength > 0.0) {
            mx = mx + (look[0] / lookHorLength * moveHorLength - mx) * 0.1;
            mz = mz + (look[2] / lookHorLength * moveHorLength - mz) * 0.1;
        }
        mx = mx * (double) 0.99F;
        my = my * (double) 0.98F;
        mz = mz * (double) 0.99F;
        return new double[] {mx, my, mz};
    }

    // travelFallFlying (client path): aiStep's velocity collapse, then the
    // elytra update, then move() with collision. Input WASD is ignored.
    static void tickElytra(World world, State s) {
        if (s.noJumpDelay > 0) s.noJumpDelay -= 1;
        double dx = s.vx, dy = s.vy, dz = s.vz;
        if (s.vx * s.vx + s.vz * s.vz < 9.0e-6) {
            dx = 0.0;
            dz = 0.0;
        }
        if (Math.abs(s.vy) < 0.003) dy = 0.0;
        double[] v = updateFallFlyingMovement(s, dx, dy, dz);
        s.vx = v[0];
        s.vy = v[1];
        s.vz = v[2];
        doMove(world, s, s.vx, s.vy, s.vz, false);
    }

    static boolean isInWater(World world, State s) {
        double[] bb = boundingBox(s);
        int minx = floor(bb[0] + 0.001), maxx = floor(bb[3] - 0.001);
        int miny = floor(bb[1] + 0.001), maxy = floor(bb[4] - 0.001);
        int minz = floor(bb[2] + 0.001), maxz = floor(bb[5] - 0.001);
        for (int x = minx; x <= maxx; x++) {
            for (int y = miny; y <= maxy; y++) {
                for (int z = minz; z <= maxz; z++) {
                    if (world.isWater(x, y, z)) return true;
                }
            }
        }
        return false;
    }

    static double effectiveGravity(double baseGravity, boolean falling, boolean slowFalling) {
        if (falling && slowFalling) {
            return Math.min(baseGravity, 0.01);
        }
        return baseGravity;
    }

    static double[] fluidFallingAdjusted(double baseGravity, boolean isFalling, boolean sprinting, double mx, double my, double mz) {
        if (baseGravity != 0.0 && !sprinting) {
            double step = baseGravity / 16.0;
            double yd;
            if (isFalling && Math.abs(my - 0.005) >= 0.003 && Math.abs(my - step) < 0.003) {
                yd = -0.003;
            } else {
                yd = my - step;
            }
            return new double[] {mx, yd, mz};
        }
        return new double[] {mx, my, mz};
    }

    static void tickWater(World world, State s, float forward, float strafe, boolean jump, boolean sneak, boolean sprint) {
        if (s.noJumpDelay > 0) s.noJumpDelay -= 1;
        double dx = s.vx, dy = s.vy, dz = s.vz;
        if (s.vx * s.vx + s.vz * s.vz < 9.0e-6) {
            dx = 0.0;
            dz = 0.0;
        }
        if (Math.abs(s.vy) < 0.003) dy = 0.0;
        s.vx = dx;
        s.vy = dy;
        s.vz = dz;
        s.sprinting = sprint;
        float[] in = modifyInput(strafe, forward, sneak);
        float xxa = in[0], zza = in[1];
        if (jump) {
            s.vy += (double) 0.04F;
        } else {
            s.noJumpDelay = 0;
        }
        boolean isFalling = s.vy <= 0.0;
        double baseGravity = effectiveGravity((double) GRAVITY, isFalling, s.slowFalling);
        float slowDown;
        if (s.dolphinsGrace) {
            slowDown = 0.96F;
        } else if (s.sprinting) {
            slowDown = WATER_SPRINT_SLOW_DOWN;
        } else {
            slowDown = WATER_SLOW_DOWN;
        }
        float speed = FLUID_INPUT_SPEED;
        double[] accel = inputVector(xxa, zza, speed, s.yaw);
        s.vx += accel[0];
        s.vy += accel[1];
        s.vz += accel[2];
        doMove(world, s, s.vx, s.vy, s.vz, sneak);
        double mvx = s.vx * (double) slowDown;
        double mvy = s.vy * (double) 0.8F;
        double mvz = s.vz * (double) slowDown;
        double[] adj = fluidFallingAdjusted(baseGravity, isFalling, s.sprinting, mvx, mvy, mvz);
        s.vx = adj[0];
        s.vy = adj[1];
        s.vz = adj[2];
    }

    static boolean isInLava(World world, State s) {
        double[] bb = boundingBox(s);
        int minx = floor(bb[0] + 0.001), maxx = floor(bb[3] - 0.001);
        int miny = floor(bb[1] + 0.001), maxy = floor(bb[4] - 0.001);
        int minz = floor(bb[2] + 0.001), maxz = floor(bb[5] - 0.001);
        for (int x = minx; x <= maxx; x++) {
            for (int y = miny; y <= maxy; y++) {
                for (int z = minz; z <= maxz; z++) {
                    if (world.isLava(x, y, z)) return true;
                }
            }
        }
        return false;
    }

    static void tickLava(World world, State s, float forward, float strafe, boolean jump, boolean sneak, boolean sprint) {
        if (s.noJumpDelay > 0) s.noJumpDelay -= 1;
        double dx = s.vx, dy = s.vy, dz = s.vz;
        if (s.vx * s.vx + s.vz * s.vz < 9.0e-6) {
            dx = 0.0;
            dz = 0.0;
        }
        if (Math.abs(s.vy) < 0.003) dy = 0.0;
        s.vx = dx;
        s.vy = dy;
        s.vz = dz;
        s.sprinting = sprint;
        float[] in = modifyInput(strafe, forward, sneak);
        float xxa = in[0], zza = in[1];
        if (jump) {
            s.vy += (double) 0.04F;
        } else {
            s.noJumpDelay = 0;
        }
        double baseGravity = GRAVITY;
        double[] accel = inputVector(xxa, zza, FLUID_INPUT_SPEED, s.yaw);
        s.vx += accel[0];
        s.vy += accel[1];
        s.vz += accel[2];
        doMove(world, s, s.vx, s.vy, s.vz, sneak);
        s.vx *= 0.5;
        s.vy *= 0.5;
        s.vz *= 0.5;
        if (baseGravity != 0.0) {
            s.vy += -baseGravity / 4.0;
        }
    }

    static void tick(World world, State s, float forward, float strafe, boolean jump, boolean sneak, boolean sprint) {
        if (isInWater(world, s)) {
            tickWater(world, s, forward, strafe, jump, sneak, sprint);
        } else if (isInLava(world, s)) {
            tickLava(world, s, forward, strafe, jump, sneak, sprint);
        } else {
            tickAir(world, s, forward, strafe, jump, sneak, sprint);
        }
    }

    // ---- Scenarios --------------------------------------------------------
    interface Scenario {
        void run(StringBuilder out);
    }

    static World flatFloor(int r) {
        World w = new World();
        for (int x = -r; x <= r; x++) {
            for (int z = -r; z <= r; z++) {
                w.addSolid(x, 0, z);
            }
        }
        return w;
    }

    static void emit(StringBuilder out, State s) {
        out.append(Long.toUnsignedString(Double.doubleToRawLongBits(s.px))).append(' ');
        out.append(Long.toUnsignedString(Double.doubleToRawLongBits(s.py))).append(' ');
        out.append(Long.toUnsignedString(Double.doubleToRawLongBits(s.pz))).append(' ');
        out.append(Long.toUnsignedString(Double.doubleToRawLongBits(s.vx))).append(' ');
        out.append(Long.toUnsignedString(Double.doubleToRawLongBits(s.vy))).append(' ');
        out.append(Long.toUnsignedString(Double.doubleToRawLongBits(s.vz))).append('\n');
    }

    static void header(StringBuilder out, String name, int ticks) {
        out.append("SCENARIO ").append(name).append(' ').append(ticks).append('\n');
    }

    public static void main(String[] args) {
        StringBuilder out = new StringBuilder();

        // free_fall
        {
            World w = new World();
            State s = new State(0.5, 100.0, 0.5, 0.0F);
            header(out, "free_fall", 200);
            for (int i = 0; i < 200; i++) {
                tickAir(w, s, 0.0F, 0.0F, false, false, false);
                emit(out, s);
            }
        }
        // walk_flat
        {
            World w = flatFloor(4);
            State s = new State(0.5, 1.0, 0.5, 0.0F);
            s.onGround = true;
            header(out, "walk_flat", 200);
            for (int i = 0; i < 200; i++) {
                tickAir(w, s, 1.0F, 0.0F, false, false, false);
                emit(out, s);
            }
        }
        // sprint_jump
        {
            World w = flatFloor(4);
            State s = new State(0.5, 1.0, 0.5, 0.0F);
            s.onGround = true;
            header(out, "sprint_jump", 120);
            for (int i = 0; i < 120; i++) {
                tickAir(w, s, 1.0F, 0.0F, true, false, true);
                emit(out, s);
            }
        }
        // ice_slide
        {
            World w = flatFloor(4);
            for (int x = -4; x <= 4; x++) {
                for (int z = -4; z <= 4; z++) {
                    w.setFriction(x, 0, z, 0.98F);
                }
            }
            State s = new State(0.5, 1.0, 0.5, 0.0F);
            s.onGround = true;
            header(out, "ice_slide", 200);
            for (int t = 0; t < 200; t++) {
                float fwd = t < 40 ? 1.0F : 0.0F;
                tickAir(w, s, fwd, 0.0F, false, false, false);
                emit(out, s);
            }
        }
        // walk_into_wall
        {
            World w = flatFloor(4);
            for (int y = 1; y < 3; y++) {
                for (int z = -2; z <= 2; z++) {
                    w.addSolid(1, y, z);
                }
            }
            State s = new State(0.5, 1.0, 0.5, 0.0F);
            s.onGround = true;
            header(out, "walk_into_wall", 80);
            for (int i = 0; i < 80; i++) {
                tickAir(w, s, 1.0F, 0.0F, false, false, false);
                emit(out, s);
            }
        }
        // slab_step
        {
            World w = flatFloor(6);
            for (int x : new int[] {1, 2, 3}) {
                for (int z = -2; z <= 2; z++) {
                    w.addBox(x, 1, z, new double[] {0.0, 0.0, 0.0, 1.0, 0.5, 1.0});
                }
            }
            State s = new State(0.5, 1.0, 0.5, 0.0F);
            s.onGround = true;
            header(out, "slab_step", 60);
            for (int i = 0; i < 60; i++) {
                tickAir(w, s, 1.0F, 0.0F, false, false, false);
                emit(out, s);
            }
        }
        // water_sink
        {
            World w = new World();
            for (int y = 80; y <= 100; y++) w.addWater(0, y, 0);
            w.addSolid(0, 78, 0);
            State s = new State(0.5, 95.0, 0.5, 0.0F);
            header(out, "water_sink", 120);
            for (int i = 0; i < 120; i++) {
                tick(w, s, 0.0F, 0.0F, false, false, false);
                emit(out, s);
            }
        }
        // diagonal_walk: strafe+forward together, exercises modifyInput's
        // two-term length (the float-rounding case axis-aligned scenarios miss).
        {
            World w = flatFloor(8);
            State s = new State(0.5, 1.0, 0.5, 0.0F);
            s.onGround = true;
            header(out, "diagonal_walk", 100);
            for (int i = 0; i < 100; i++) {
                tickAir(w, s, 1.0F, 1.0F, false, false, false);
                emit(out, s);
            }
        }

        // analog_strafe: asymmetric analog input (0.5 fwd, 1.0 strafe) so
        // sx != sy and modifyInput's two-term length actually rounds
        // differently under per-op vs single-cast. Proves Rust/JVM per-op agree.
        {
            World w = flatFloor(8);
            State s = new State(0.5, 1.0, 0.5, 0.0F);
            s.onGround = true;
            header(out, "analog_strafe", 100);
            for (int i = 0; i < 100; i++) {
                tickAir(w, s, 0.5F, 1.0F, false, false, false);
                emit(out, s);
            }
        }

        // ladder_climb: hold jump on a vertical ladder column; steady climb-up.
        {
            World w = new World();
            for (int y = 0; y < 16; y++) w.addClimbable(0, y, 0);
            State s = new State(0.5, 2.0, 0.5, 0.0F);
            s.onGround = false;
            header(out, "ladder_climb", 80);
            for (int i = 0; i < 80; i++) {
                tickAir(w, s, 0.0F, 0.0F, true, false, false);
                emit(out, s);
            }
        }

        // ladder_sneak_hold: sneaking on a ladder holds vertical position
        // (yd forced to 0 while descending) instead of sliding down.
        {
            World w = new World();
            for (int y = 0; y < 16; y++) w.addClimbable(0, y, 0);
            State s = new State(0.5, 8.0, 0.5, 0.0F);
            s.onGround = false;
            s.vy = -0.2;
            header(out, "ladder_sneak_hold", 60);
            for (int i = 0; i < 60; i++) {
                tickAir(w, s, 0.0F, 0.0F, false, true, false);
                emit(out, s);
            }
        }

        // blue_ice_slide: slipperiest tier (friction 0.989F) — longer glide than
        // ice/packed_ice (0.98F). Confirms the friction path handles all tiers.
        {
            World w = flatFloor(4);
            for (int x = -4; x <= 4; x++) {
                for (int z = -4; z <= 4; z++) {
                    w.setFriction(x, 0, z, 0.989F);
                }
            }
            State s = new State(0.5, 1.0, 0.5, 0.0F);
            s.onGround = true;
            header(out, "blue_ice_slide", 200);
            for (int t = 0; t < 200; t++) {
                float fwd = t < 40 ? 1.0F : 0.0F;
                tickAir(w, s, fwd, 0.0F, false, false, false);
                emit(out, s);
            }
        }

        // lava_sink: deep lava column, no input. Exercises tickLava's deep branch
        // (moveRelative(0.02) → move → scale(0.5) → -baseGravity/4).
        {
            World w = new World();
            for (int y = 80; y <= 100; y++) w.addLava(0, y, 0);
            w.addSolid(0, 78, 0);
            State s = new State(0.5, 95.0, 0.5, 0.0F);
            header(out, "lava_sink", 120);
            for (int i = 0; i < 120; i++) {
                tick(w, s, 0.0F, 0.0F, false, false, false);
                emit(out, s);
            }
        }

        // levitation: amplifier 0 in open air; gravity is replaced by a pull
        // toward 0.05*(amp+1)=0.05, so the player rises.
        {
            World w = new World();
            State s = new State(0.5, 100.0, 0.5, 0.0F);
            s.levitation = 0;
            header(out, "levitation", 120);
            for (int i = 0; i < 120; i++) {
                tick(w, s, 0.0F, 0.0F, false, false, false);
                emit(out, s);
            }
        }

        // slow_falling_water: Slow Falling reduces getEffectiveGravity() to 0.01
        // while descending, so baseGravity/16 = 0.000625 != 0.005 and the
        // otherwise provably-dead -0.003 slow-sink clamp fires.
        {
            World w = new World();
            for (int y = 80; y <= 100; y++) w.addWater(0, y, 0);
            w.addSolid(0, 78, 0);
            State s = new State(0.5, 95.0, 0.5, 0.0F);
            s.slowFalling = true;
            header(out, "slow_falling_water", 120);
            for (int i = 0; i < 120; i++) {
                tick(w, s, 0.0F, 0.0F, false, false, false);
                emit(out, s);
            }
        }

        // swim_sprint: sprint-swimming with forward input — the swimming branch of
        // travelInWater (slowDown = 0.9F), not exercised by the vertical sink.
        {
            World w = new World();
            for (int y = 80; y <= 100; y++) {
                for (int x = -2; x <= 2; x++) {
                    for (int z = -2; z <= 2; z++) {
                        w.addWater(x, y, z);
                    }
                }
            }
            State s = new State(0.5, 90.0, 0.5, 0.0F);
            header(out, "swim_sprint", 120);
            for (int i = 0; i < 120; i++) {
                tick(w, s, 1.0F, 0.0F, false, false, true);
                emit(out, s);
            }
        }

        // soul_sand_walk: full collision cube carrying block speed factor 0.4.
        // Player rests at y=1.0 so blockPosition() is air (1.0) and the factor
        // falls through to the block below — the here==1.0 fallback branch.
        {
            World w = flatFloor(8);
            for (int x = -8; x <= 8; x++) {
                for (int z = -8; z <= 8; z++) {
                    w.setSpeedFactor(x, 0, z, 0.4F);
                }
            }
            State s = new State(0.5, 1.0, 0.5, 0.0F);
            s.onGround = true;
            header(out, "soul_sand_walk", 120);
            for (int i = 0; i < 120; i++) {
                tick(w, s, 1.0F, 0.0F, false, false, false);
                emit(out, s);
            }
        }

        // jump_boost: Jump Boost II (amp 1) adds 0.1F*(1+1)=0.2F to jump velocity
        // as a separate float term in getJumpPower.
        {
            World w = flatFloor(4);
            State s = new State(0.5, 1.0, 0.5, 0.0F);
            s.onGround = true;
            s.jumpBoost = 1;
            header(out, "jump_boost", 120);
            for (int i = 0; i < 120; i++) {
                tick(w, s, 0.0F, 0.0F, true, false, false);
                emit(out, s);
            }
        }

        // honey_jump: block jump factor 0.5 scales jump velocity to 0.42*0.5.
        {
            World w = flatFloor(4);
            for (int x = -4; x <= 4; x++) {
                for (int z = -4; z <= 4; z++) {
                    w.setJumpFactor(x, 0, z, 0.5F);
                }
            }
            State s = new State(0.5, 1.0, 0.5, 0.0F);
            s.onGround = true;
            header(out, "honey_jump", 120);
            for (int i = 0; i < 120; i++) {
                tick(w, s, 0.0F, 0.0F, true, false, false);
                emit(out, s);
            }
        }

        // slime_bounce: free-fall onto slime (bounceRestitution 1.0); restitute
        // reverses vy through the block-bounciness branch.
        {
            World w = flatFloor(4);
            for (int x = -4; x <= 4; x++) {
                for (int z = -4; z <= 4; z++) {
                    w.addSlime(x, 0, z);
                }
            }
            State s = new State(0.5, 6.0, 0.5, 0.0F);
            header(out, "slime_bounce", 160);
            for (int i = 0; i < 160; i++) {
                tick(w, s, 0.0F, 0.0F, false, false, false);
                emit(out, s);
            }
        }

        // slime_bounce_sneak: holding sneak (isSuppressingBounce) vetoes the
        // block-bounce branch, so the player lands and rests instead of bouncing.
        {
            World w = flatFloor(4);
            for (int x = -4; x <= 4; x++) {
                for (int z = -4; z <= 4; z++) {
                    w.addSlime(x, 0, z);
                }
            }
            State s = new State(0.5, 6.0, 0.5, 0.0F);
            header(out, "slime_bounce_sneak", 160);
            for (int i = 0; i < 160; i++) {
                tick(w, s, 0.0F, 0.0F, false, true, false);
                emit(out, s);
            }
        }

        // ---- Elytra (travelFallFlying) ------------------------------------
        // Direction comes purely from look angle; WASD is ignored. Each seeds a
        // forward velocity so moveHorLength > 0 and the redistribution terms
        // (which vanish at rest) are exercised from tick 0.

        // elytra_glide_level: pitch 0, yaw 0 → level launch. liftForce = 1,
        // so vertical gain is gravity*(-1 + 0.75) = -0.02/tick before the
        // horizontal->look realignment. Pure +Z glide, slow sink.
        {
            World w = new World();
            State s = new State(0.5, 100.0, 0.5, 0.0F);
            s.pitch = 0.0F;
            s.fallFlying = true;
            s.vz = 1.0;
            header(out, "elytra_glide_level", 160);
            for (int i = 0; i < 160; i++) {
                tickElytra(w, s);
                emit(out, s);
            }
        }

        // elytra_dive: pitch +37° (nose-down, off-grid angle). my goes negative
        // fast, firing the `my < 0` convert branch that trades altitude for
        // speed. Off-nice-angle pitch forces LUT-cos vs Math.cos to matter.
        {
            World w = new World();
            State s = new State(0.5, 200.0, 0.5, 0.0F);
            s.pitch = 37.0F;
            s.fallFlying = true;
            s.vz = 0.8;
            header(out, "elytra_dive", 160);
            for (int i = 0; i < 160; i++) {
                tickElytra(w, s);
                emit(out, s);
            }
        }

        // elytra_climb: pitch -23° (nose-up). Exercises the `leanAngle < 0`
        // branch: -Mth.sin(leanAngle) is positive, adding convert*3.2 lift and a
        // backward horizontal component. The classic pump-up phase then stall.
        {
            World w = new World();
            State s = new State(0.5, 100.0, 0.5, 0.0F);
            s.pitch = -23.0F;
            s.fallFlying = true;
            s.vz = 1.4;
            header(out, "elytra_climb", 160);
            for (int i = 0; i < 160; i++) {
                tickElytra(w, s);
                emit(out, s);
            }
        }

        // elytra_diagonal_yaw: yaw 33°, pitch +11° so look.x and look.z are both
        // nonzero and unequal. The `look/lookHorLength * moveHorLength - m` steer
        // redistributes speed onto both axes with different rounding per axis —
        // an axis-asymmetric case a pure +Z glide can't reach.
        {
            World w = new World();
            State s = new State(0.5, 150.0, 0.5, 33.0F);
            s.pitch = 11.0F;
            s.fallFlying = true;
            s.vx = 0.3;
            s.vz = 0.9;
            header(out, "elytra_diagonal_yaw", 160);
            for (int i = 0; i < 160; i++) {
                tickElytra(w, s);
                emit(out, s);
            }
        }

        System.out.print(out);
    }
}
