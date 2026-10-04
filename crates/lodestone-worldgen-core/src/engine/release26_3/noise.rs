//! Current noise parameters and mixed-precision scalar sampling.
//!
//! Coordinates, frequencies and normalization calculations are double precision.
//! Lattice fractions, gradients, interpolation and layer accumulation are single
//! precision. Rounding only the final output would lose those boundaries.

use serde_json::Value;

use crate::rng::{Algorithm, PositionalRandomFactory, RandomSource};

use super::{BuildError, SampleVolume};

const SECOND_FREQUENCY: f64 = 1.018_126_888_217_522_7;
const OCTAVE_DEVIATION: f64 = 0.270_224_783_124_521_1;
pub(super) const GRADIENTS: [[f32; 3]; 16] = [
    [1.0, 1.0, 0.0], [-1.0, 1.0, 0.0], [1.0, -1.0, 0.0], [-1.0, -1.0, 0.0],
    [1.0, 0.0, 1.0], [-1.0, 0.0, 1.0], [1.0, 0.0, -1.0], [-1.0, 0.0, -1.0],
    [0.0, 1.0, 1.0], [0.0, -1.0, 1.0], [0.0, 1.0, -1.0], [0.0, -1.0, -1.0],
    [1.0, 1.0, 0.0], [0.0, -1.0, 1.0], [-1.0, 1.0, 0.0], [0.0, -1.0, -1.0],
];

#[derive(Clone, Debug)]
struct Perlin {
    permutation: [u8; 256],
    offset: [f64; 3],
    smear: f64,
}

impl Perlin {
    fn new(random: &mut impl RandomSource) -> Self {
        let offset = std::array::from_fn(|_| random.next_double() * 256.0);
        let mut permutation = std::array::from_fn(|index| index as u8);
        for index in 0..256 {
            let offset = random.next_int_bounded((256 - index) as i32) as usize;
            permutation.swap(index, index + offset);
        }
        Self { permutation, offset, smear:0.0 }
    }

    fn permute(&self, index: i32) -> i32 {
        i32::from(self.permutation[(index & 255) as usize])
    }

    fn sample(&self, position: [f64; 3]) -> f32 {
        let raw_y=position[1];
        let position = std::array::from_fn::<_, 3, _>(|axis| wrap(position[axis]) + self.offset[axis]);
        let lattice = position.map(|value| value.floor() as i32);
        let mut fraction = std::array::from_fn::<_, 3, _>(|axis| (position[axis] - f64::from(lattice[axis])) as f32);
        let smooth=fraction.map(smoothstep);
        if self.smear!=0.0 {
            let y=position[1]-f64::from(lattice[1]);
            fraction[1]=(y-self.vertical_displacement(raw_y,y)) as f32;
        }
        let mut corners = [0.0; 8];
        for (index, corner) in corners.iter_mut().enumerate() {
            let x = (index & 1) as i32;
            let y = ((index >> 1) & 1) as i32;
            let z = ((index >> 2) & 1) as i32;
            let hash = self.permute(self.permute(self.permute(lattice[0] + x) + lattice[1] + y) + lattice[2] + z);
            let gradient = GRADIENTS[(hash & 15) as usize];
            *corner = (gradient[0] * (fraction[0] - x as f32)
                + gradient[1] * (fraction[1] - y as f32))
                + gradient[2] * (fraction[2] - z as f32);
        }
        let [x, y, z] = smooth;
        lerp(z,
            lerp(y, lerp(x, corners[0], corners[1]), lerp(x, corners[2], corners[3])),
            lerp(y, lerp(x, corners[4], corners[5]), lerp(x, corners[6], corners[7])))
    }

    fn vertical_displacement(&self,raw_y:f64,fraction:f64)->f64 {
        let limited=if raw_y>=0.0 && raw_y<fraction {raw_y} else {fraction};
        ((limited/self.smear+f64::from(1.0e-7_f32)).floor() as i32) as f64*self.smear
    }

    fn add_volume(&self, volume: SampleVolume, output: &mut [f32], xz_scale: f64, y_scale: f64, amplitude: f32) {
        let mut output_index = 0;
        for z in 0..volume.size[2] {
            let z = wrap(f64::from(volume.coordinate(2,z)) * xz_scale) + self.offset[2];
            let iz = z.floor() as i32;
            let fz = (z - f64::from(iz)) as f32;
            let sz = smoothstep(fz);
            for x in 0..volume.size[0] {
                let x = wrap(f64::from(volume.coordinate(0,x)) * xz_scale) + self.offset[0];
                let ix = x.floor() as i32;
                let fx = (x - f64::from(ix)) as f32;
                let sx = smoothstep(fx);
                let mut cached_y = i32::MIN;
                let mut horizontal = [0.0;8];
                let mut vertical = [0.0;8];
                for y in 0..volume.size[1] {
                    let raw_y=f64::from(volume.coordinate(1,y))*y_scale;
                    let y = wrap(raw_y) + self.offset[1];
                    let iy = y.floor() as i32;
                    let fraction=y-f64::from(iy);
                    let sy=smoothstep(fraction as f32);
                    let fy=if self.smear==0.0 {fraction as f32}
                        else {(fraction-self.vertical_displacement(raw_y,fraction)) as f32};
                    if iy != cached_y {
                        for corner in 0..8 {
                            let dx = (corner & 1) as i32;
                            let dy = ((corner >> 1) & 1) as i32;
                            let dz = ((corner >> 2) & 1) as i32;
                            let hash = self.permute(self.permute(self.permute(ix + dx) + iy + dy) + iz + dz);
                            let gradient = GRADIENTS[(hash & 15) as usize];
                            horizontal[corner] = gradient[0] * (fx - dx as f32) + gradient[2] * (fz - dz as f32);
                            vertical[corner] = gradient[1];
                        }
                        cached_y = iy;
                    }
                    let corners = std::array::from_fn::<_,8,_>(|corner| horizontal[corner]
                        + vertical[corner] * (fy - ((corner >> 1) & 1) as f32));
                    let value = lerp(sz,
                        lerp(sy,lerp(sx,corners[0],corners[1]),lerp(sx,corners[2],corners[3])),
                        lerp(sy,lerp(sx,corners[4],corners[5]),lerp(sx,corners[6],corners[7])));
                    output[output_index] += amplitude * value;
                    output_index += 1;
                }
            }
        }
    }
}

fn wrap(value: f64) -> f64 {
    const PERIOD: f64 = 33_554_432.0;
    let half = 16_777_216.0_f64.next_down();
    if (-half..half).contains(&value) { value }
    else { value - (value / PERIOD + 0.5).floor() * PERIOD }
}

fn smoothstep(value: f32) -> f32 {
    ((value * value) * value) * (value * (value * 6.0 - 15.0) + 10.0)
}

fn lerp(alpha: f32, first: f32, second: f32) -> f32 {
    first + alpha * (second - first)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Normalization { Enabled, Disabled, Legacy }

/// Validated octave metadata. No resource names or JSON remain in this value.
#[derive(Clone, Debug)]
pub struct NoiseParameters {
    base_amplitude: f64,
    base_octave: i32,
    octave_count: usize,
    normalization: Normalization,
    modifiers: Vec<f64>,
}

#[derive(Clone, Debug)]
struct Layer {
    octave: Perlin,
    frequency: f64,
    amplitude: f32,
}

/// Seeded, immutable scalar noise layers in their accumulation order.
#[derive(Clone, Debug)]
pub struct NoiseSampler {
    layers: Vec<Layer>,
}

impl NoiseParameters {
    pub fn parse(value: &Value) -> Result<Self, BuildError> {
        let path = "noise";
        let base_octave = value["base_octave"].as_i64().filter(|v| (-32..=32).contains(v))
            .ok_or_else(|| BuildError::new(path, "base_octave must be in [-32, 32]"))? as i32;
        let base_amplitude = optional_number(value, "base_amplitude", 1.0)?;
        if !(9.999_999_747_378_752e-6..=1_000_000.0).contains(&base_amplitude) {
            return Err(BuildError::new(path, "base_amplitude is outside its supported range"));
        }
        let octave_count = match value.get("octave_count") {
            None => 1,
            Some(value) => value.as_u64().filter(|v| (1..=32).contains(v))
                .ok_or_else(|| BuildError::new(path, "octave_count must be in [1, 32]"))? as usize,
        };
        let normalization = match value.get("normalize") {
            None | Some(Value::Bool(true)) => Normalization::Enabled,
            Some(Value::Bool(false)) => Normalization::Disabled,
            Some(Value::String(value)) if value == "legacy" => Normalization::Legacy,
            _ => return Err(BuildError::new(path, "normalize must be true, false or legacy")),
        };
        let modifiers = match value.get("amplitude_modifiers") {
            None => Vec::new(),
            Some(value) => value.as_array().filter(|a| a.is_empty() || a.len() == octave_count)
                .ok_or_else(|| BuildError::new(path, "amplitude_modifiers must be empty or match octave_count"))?
                .iter().map(|value| value.as_f64().filter(|v| (0.0..=1_000_000.0).contains(v))
                    .ok_or_else(|| BuildError::new(path, "amplitude modifier must be in [0, 1000000]")))
                .collect::<Result<Vec<_>, _>>()?,
        };
        Ok(Self { base_amplitude, base_octave, octave_count, normalization, modifiers })
    }

    fn modifier(&self, index: usize) -> f64 {
        if self.modifiers.is_empty() { 1.0 } else { self.modifiers[index] }
    }

    fn octaves(&self) -> (Vec<(i32, f64, f64)>, f64) {
        let mut frequency = 2.0_f64.powi(self.base_octave);
        let mut amplitude = self.base_amplitude;
        if self.normalization != Normalization::Disabled {
            amplitude *= 2.0_f64.powi(self.octave_count as i32 - 1)
                / (2.0_f64.powi(self.octave_count as i32) - 1.0);
        }
        let mut octaves = Vec::with_capacity(self.octave_count);
        for index in 0..self.octave_count {
            let modifier = self.modifier(index);
            if modifier != 0.0 { octaves.push((self.base_octave + index as i32, frequency, amplitude * modifier)); }
            frequency *= 2.0;
            amplitude *= 0.5;
        }
        let absolute_sum = compensated_sum(octaves.iter().map(|(_, _, amplitude)| amplitude.abs()));
        let variance = octaves.iter().fold(0.0, |sum, (_, _, amplitude)| {
            let deviation = OCTAVE_DEVIATION * amplitude.abs();
            sum + deviation * deviation
        });
        let deviation = variance.sqrt();
        let mut factor = if deviation == 0.0 { 0.0 }
            else { (absolute_sum * (1.0 / 3.0)) / (deviation * 2.0_f64.sqrt()) };
        if factor != 0.0 && self.normalization == Normalization::Legacy {
            let first = (0..self.octave_count).find(|&i| self.modifier(i) != 0.0).unwrap();
            let last = (0..self.octave_count).rfind(|&i| self.modifier(i) != 0.0).unwrap();
            let expected = 0.1 * (1.0 + 1.0 / (last - first + 1) as f64);
            factor = (self.base_amplitude * 0.5 * (1.0 / 3.0)) / expected;
        }
        (octaves, factor)
    }

    /// Seeds one named resource from the world's positional factory. The two
    /// Nether climate resources retain their independent raw-seed schedules.
    pub fn instantiate(&self, seed: i64, algorithm: Algorithm, id: &str) -> Result<NoiseSampler, BuildError> {
        let offset = match id {
            "minecraft:nether/temperature" => Some(0),
            "minecraft:nether/vegetation" => Some(1),
            _ => None,
        };
        if let Some(offset) = offset {
            let mut random = crate::rng::LegacyRandomSource::new(seed.wrapping_add(offset));
            return self.instantiate_legacy_climate(&mut random);
        }
        let mut random = algorithm.root_positional(seed).from_hash_of(id);
        Ok(self.instantiate_from_random(&mut random))
    }

    /// Builds ordinary normal noise from an already-selected random stream.
    pub fn instantiate_from_random(&self, random: &mut impl RandomSource) -> NoiseSampler {
        let first = random.fork_positional();
        let second = random.fork_positional();
        let (octaves, factor) = self.octaves();
        let mut layers = Vec::with_capacity(octaves.len() * 2);
        for (index, frequency, amplitude) in octaves {
            let key = format!("octave_{index}");
            let amplitude = (factor * amplitude) as f32;
            layers.push(Layer { octave: Perlin::new(&mut first.from_hash_of(&key)), frequency, amplitude });
            layers.push(Layer { octave: Perlin::new(&mut second.from_hash_of(&key)), frequency: frequency * SECOND_FREQUENCY, amplitude });
        }
        NoiseSampler { layers }
    }

    fn legacy_layers(&self, random: &mut impl RandomSource) -> Result<Vec<Layer>, BuildError> {
        let zero_index = -self.base_octave;
        if zero_index < self.octave_count as i32 - 1 {
            return Err(BuildError::new("noise", "legacy climate noise requires nonpositive octaves"));
        }
        let mut octaves: Vec<Option<Perlin>> = vec![None; self.octave_count];
        let initial = Perlin::new(random);
        if let Ok(index) = usize::try_from(zero_index) {
            if index < self.octave_count && self.modifier(index) != 0.0 { octaves[index] = Some(initial); }
        }
        for index in (0..zero_index).rev() {
            if (index as usize) < self.octave_count && self.modifier(index as usize) != 0.0 {
                octaves[index as usize] = Some(Perlin::new(random));
            } else { random.consume_count(262); }
        }
        let mut frequency = 2.0_f64.powi(self.base_octave);
        let mut amplitude = 2.0_f64.powi(self.octave_count as i32 - 1)
            / (2.0_f64.powi(self.octave_count as i32) - 1.0);
        let mut layers = Vec::new();
        for (index, octave) in octaves.into_iter().enumerate() {
            if let Some(octave) = octave { layers.push(Layer { octave, frequency, amplitude: (amplitude * self.modifier(index)) as f32 }); }
            frequency *= 2.0;
            amplitude /= 2.0;
        }
        Ok(layers)
    }

    fn instantiate_legacy_climate(&self, random: &mut impl RandomSource) -> Result<NoiseSampler, BuildError> {
        let mut first = self.legacy_layers(random)?;
        let mut second = self.legacy_layers(random)?;
        let (_, factor) = self.octaves();
        let multiplier = (factor * self.base_amplitude) as f32;
        for layer in &mut first { layer.amplitude *= multiplier; }
        for layer in &mut second { layer.frequency *= SECOND_FREQUENCY; layer.amplitude *= multiplier; }
        first.extend(second);
        Ok(NoiseSampler { layers: first })
    }
}

impl NoiseSampler {
    pub(super) fn smeared(random:&mut impl RandomSource,count:i32,smear:f64,normalization:f64)->Self {
        let mut frequency=1.0;
        let mut amplitude=normalization/(2.0_f64.powi(count)-1.0);
        let mut layers=Vec::with_capacity(count as usize);
        for _ in 0..count {
            let mut octave=Perlin::new(random);
            octave.smear=smear*frequency;
            layers.push(Layer {octave,frequency,amplitude:amplitude as f32});
            frequency/=2.0;amplitude*=2.0;
        }
        Self {layers}
    }

    #[must_use]
    pub fn sample(&self, x: f64, y: f64, z: f64) -> f32 {
        self.layers.iter().fold(0.0, |sum, layer| {
            sum + layer.amplitude * layer.octave.sample([x * layer.frequency, y * layer.frequency, z * layer.frequency])
        })
    }

    /// Adds a regular volume to caller-owned storage without allocations.
    /// Outer scales and amplitude combine with each layer before lattice work.
    pub fn add_volume(&self, volume: SampleVolume, output: &mut [f32], xz_scale: f64,
        y_scale: f64, amplitude: f32) -> Result<(), BuildError> {
        volume.check_output(output)?;
        if !xz_scale.is_finite() || !y_scale.is_finite() || !amplitude.is_finite() {
            return Err(BuildError::new("volume", "noise scales and amplitude must be finite"));
        }
        for layer in &self.layers {
            layer.octave.add_volume(volume, output, xz_scale * layer.frequency,
                y_scale * layer.frequency, amplitude * layer.amplitude);
        }
        Ok(())
    }
}

pub(super) fn permutation(random:&mut impl RandomSource)->[u8;256] {Perlin::new(random).permutation}

fn optional_number(value: &Value, key: &str, default: f64) -> Result<f64, BuildError> {
    match value.get(key) {
        None => Ok(default),
        Some(value) => value.as_f64().filter(|v| v.is_finite())
            .ok_or_else(|| BuildError::new("noise", format!("{key} must be a finite number"))),
    }
}

fn compensated_sum(values: impl Iterator<Item = f64>) -> f64 {
    let (mut sum, mut correction) = (0.0, 0.0);
    for value in values {
        let adjusted = value - correction;
        let next = sum + adjusted;
        correction = (next - sum) - adjusted;
        sum = next;
    }
    sum - correction
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::LegacyRandomSource;
    use serde_json::json;

    #[test]
    fn constructor_and_scalar_samples_match_independent_release_witnesses() {
        let positions = [[0.125, 17.375, -9.625], [-37.0625, -1.125, 83.8125],
            [16_777_215.75, -77.3, 33_554_432.125]];
        let fixtures = [
            (0, [0x406764168ea6ca89, 0x404ec9e5b3672e14, 0x406465b93a78ef81],
                [140,157,53,179,117,253,229,104,247,35,20,221,25,51,211,18],
                [0xbd83998c, 0xbca7e860, 0x3eceeab5]),
            (937, [0x4066e8113a6211ee, 0x406c97dfefee3e23, 0x40121b24340a7080],
                [193,251,111,242,68,200,55,69,78,189,234,106,102,116,230,48],
                [0xbebce13c, 0xbe072d68, 0x3e7c4627]),
            (-18413, [0x406b2f3b8d14b770, 0x4062452848482d2e, 0x4055667ae26e443c],
                [222,94,50,140,135,165,210,175,216,120,106,56,137,121,31,189],
                [0x3e8bc77f, 0x3e58bb37, 0xbd313074]),
        ];
        for (seed, offsets, prefix, samples) in fixtures {
            let noise = Perlin::new(&mut LegacyRandomSource::new(seed));
            assert_eq!(noise.offset.map(f64::to_bits), offsets, "seed {seed}");
            assert_eq!(noise.permutation[..16], prefix, "seed {seed}");
            for (position, expected) in positions.into_iter().zip(samples) {
                assert_eq!(noise.sample(position).to_bits(), expected, "seed {seed} at {position:?}");
            }
        }
    }

    #[test]
    fn noise_schema_rejects_wrong_octave_lengths_and_bounds() {
        for value in [json!({"base_octave":-33}), json!({"base_octave":0,"octave_count":0}),
            json!({"base_octave":0,"octave_count":2,"amplitude_modifiers":[1]}),
            json!({"base_octave":0,"normalize":"other"}), json!({"base_octave":0,"base_amplitude":0})] {
            assert!(NoiseParameters::parse(&value).is_err(), "{value}");
        }
    }

    #[test]
    fn normal_layers_match_independent_seed_schedule_and_accumulation_witnesses() {
        let parameters = NoiseParameters::parse(&json!({"base_octave":-3,"octave_count":2,
            "base_amplitude":0.75})).unwrap();
        let positions = [[13.125,-37.25,21.0625],[-163.75,63.125,819.5]];
        for (seed, expected) in [(0,[0xbe9463ca,0x3e8b0d60]),
            (937,[0xbe4828e2,0xbe87d232]),(-18413,[0xbe219c88,0x3ea572d2])] {
            let noise = parameters.instantiate_from_random(&mut LegacyRandomSource::new(seed));
            assert_eq!(noise.layers.iter().map(|layer| layer.amplitude.to_bits()).collect::<Vec<_>>(),
                [0x3f15ca6a,0x3f15ca6a,0x3e95ca6a,0x3e95ca6a]);
            for ([x,y,z], expected) in positions.into_iter().zip(expected) {
                assert_eq!(noise.sample(x,y,z).to_bits(), expected, "seed {seed} at {:?}", [x,y,z]);
            }
        }
    }

    #[test]
    fn shift_mappings_match_independent_asymmetric_coordinate_witnesses() {
        use super::super::{BlockContext, EvalWorkspace, Node, NodeId, Program, Shift};
        for (mapping, expected) in [(Shift::ThreeDimensional,0xbf0c4240),
            (Shift::Horizontal,0xc014fbf0),(Shift::RotatedHorizontal,0xbe5b930f)] {
            let noise = NoiseSampler { layers:vec![Layer { octave:Perlin::new(&mut LegacyRandomSource::new(0)),
                frequency:1.0, amplitude:1.0 }] };
            let program = Program { nodes:vec![Node::Shift {sampler:0,mapping}], roots:vec![NodeId(0)],
                effects:vec![false],cache_count:0,noises:vec![noise],islands:None,tables:Default::default(),scratch_frames:1,scalar_capacity:0 };
            let value = program.sample(BlockContext {x:-149,y:63,z:334}, &mut EvalWorkspace::new(&program)).unwrap();
            assert_eq!(value.to_bits(), expected, "{mapping:?}");
        }
    }

    #[test]
    fn volume_layer_grouping_matches_independent_bulk_witness() {
        let volume = SampleVolume::new(super::super::BlockContext {x:-149,y:63,z:334}, [2,3,2], [3,1,7]).unwrap();
        let parameters = NoiseParameters::parse(&json!({"base_octave":-3,"octave_count":2,
            "base_amplitude":0.75})).unwrap();
        let noise = parameters.instantiate_from_random(&mut LegacyRandomSource::new(-18413));
        let mut output = [0.0;12];
        noise.add_volume(volume, &mut output, 0.137,0.193,1.37).unwrap();
        assert_eq!(output.map(f32::to_bits), [0x3ea498ca,0x3eb5f974,0x3ec5c2ce,0x3e9f9936,
            0x3eb45f62,0x3ec6fcf5,0x3e867bbd,0x3e968aac,0x3ea5c3b0,0x3e7ec4a7,0x3e90b566,0x3ea0aa2e]);
        let mut wrong = [0.0;12];
        let mut index = 0;
        for z in 0..2 { for x in 0..2 { for y in 0..3 {
            wrong[index] = 1.37 * noise.sample(f64::from(volume.coordinate(0,x))*0.137,
                f64::from(volume.coordinate(1,y))*0.193,f64::from(volume.coordinate(2,z))*0.137);
            index += 1;
        } } }
        assert_eq!(wrong.map(f32::to_bits), [0x3ea498ca,0x3eb5f975,0x3ec5c2cc,0x3e9f9936,
            0x3eb45f62,0x3ec6fcf5,0x3e867bbc,0x3e968aac,0x3ea5c3b0,0x3e7ec4a7,0x3e90b566,0x3ea0aa2f]);
        assert_ne!(output.map(f32::to_bits), wrong.map(f32::to_bits));
        noise.add_volume(volume, &mut output,0.137,0.193,0.0).unwrap();
        assert_eq!(output[0].to_bits(),0x3ea498ca);
    }

    #[test]
    fn volume_coordinate_scale_groups_before_integer_multiplication() {
        let noise = NoiseSampler { layers:vec![Layer { octave:Perlin::new(&mut LegacyRandomSource::new(937)),
            frequency:0.125 * SECOND_FREQUENCY, amplitude:1.0 }] };
        let position = super::super::BlockContext {x:2_147_483_123,y:63,z:2_147_483_440};
        let volume = SampleVolume::new(position,[1;3],[1;3]).unwrap();
        let mut output = [0.0];
        noise.add_volume(volume,&mut output,1.37,0.193,1.0).unwrap();
        assert_eq!(output[0].to_bits(),0x3e074375);
        let wrong = noise.sample(f64::from(position.x)*1.37,f64::from(position.y)*0.193,f64::from(position.z)*1.37);
        assert_eq!(wrong.to_bits(),0x3e074372);
    }

    #[test]
    fn interpolation_preserves_distinct_scalar_and_bulk_release_witnesses() {
        use super::super::{BlockContext,EvalWorkspace,Node,NodeId,Program};
        let parameters = NoiseParameters::parse(&json!({"base_octave":-3,"octave_count":2,"base_amplitude":0.75})).unwrap();
        let noise = parameters.instantiate_from_random(&mut LegacyRandomSource::new(937));
        let mut program = Program {nodes:vec![Node::Noise {sampler:0,xz_scale:0.137,y_scale:0.193,shifts:[None;3]},
            Node::Interpolated {input:NodeId(0),xz:4,y:7,inverse_xz:0.25,inverse_y:1.0/7.0}],
            roots:vec![NodeId(1)],effects:vec![false;2],cache_count:0,
            noises:vec![noise],islands:None,tables:Default::default(),scratch_frames:2,scalar_capacity:8};
        let volume = SampleVolume::new(BlockContext{x:-3,y:-5,z:1},[3,5,2],[1;3]).unwrap();
        let mut workspace = EvalWorkspace::new(&program);
        assert!(program.sample(volume.min,&mut workspace).is_err());
        workspace.prepare_volume(&program,volume,256).unwrap();
        let mut output=[0.0;30];
        program.sample_volume(volume,&mut output,&mut workspace).unwrap();
        assert_eq!(output.map(f32::to_bits),[0xbe868341,0xbe8935f5,0xbe8be8a9,0xbe8e9b5d,0xbe914e11,
            0xbe938bc9,0xbe967a97,0xbe996965,0xbe9c5833,0xbe9f4701,0xbea09453,0xbea3bf3a,0xbea6ea21,
            0xbeaa1508,0xbead3fef,0xbe776205,0xbe7c6d35,0xbe80bc32,0xbe8341ca,0xbe85c762,0xbe892833,
            0xbe8be813,0xbe8ea7f3,0xbe9167d3,0xbe9427b3,0xbe969f63,0xbe99998c,0xbe9c93b5,0xbe9f8dde,0xbea28807]);
        for (position,bits) in [(volume.min,0xbe868341),(BlockContext::default(),0xbec922f5),
            (BlockContext{x:-1,y:-1,z:2},0xbea28806)] {
            assert_eq!(program.sample(position,&mut workspace).unwrap().to_bits(),bits,"{position:?}");
        }
        assert_ne!(output[29].to_bits(),0xbea28806);
        for (minimum,size,step,expected) in [
            (BlockContext{x:-3,y:-5,z:1},[2,2,2],[3,2,5],
                [0xbe868341,0xbe8be8a9,0xbead9cdc,0xbeb46ade,0xbe1dddef,0xbe23f9f3,0xbe73d02d,0xbe7c777d]),
            (BlockContext{x:-4,y:-7,z:0},[2,2,2],[4,7,4],
                [0xbe7eefa2,0xbe91e51c,0xbeaff004,0xbec922f5,0xbe279d2a,0xbe42ff44,0xbe8b6b60,0xbe9f169d]),
        ] {
            let volume=SampleVolume::new(minimum,size,step).unwrap();
            workspace.prepare_volume(&program,volume,2048).unwrap();
            let mut output=[0.0;8];
            program.sample_volume(volume,&mut output,&mut workspace).unwrap();
            assert_eq!(output.map(f32::to_bits),expected,"step {step:?}");
        }
        program.nodes[1]=Node::Interpolated {input:NodeId(0),xz:16,y:1,inverse_xz:1.0/16.0,inverse_y:1.0};
        let volume=SampleVolume::new(BlockContext{x:-3,y:-2,z:1},[2,3,2],[1;3]).unwrap();
        workspace.prepare_volume(&program,volume,256).unwrap();
        let mut output=[0.0;12];
        program.sample_volume(volume,&mut output,&mut workspace).unwrap();
        assert_eq!(output.map(f32::to_bits),[0xbea17ffa,0xbea191aa,0xbe9fdcbe,0xbeaa43b4,0xbeaa32e1,0xbea87a84,
            0xbe92bb2e,0xbe929503,0xbe90ccda,0xbe9b5720,0xbe9b1280,0xbe994ba1]);
        assert_eq!(program.sample(BlockContext{x:-3,y:-5,z:1},&mut workspace).unwrap().to_bits(),0xbe953dc4);
    }

    #[test]
    fn cache_replacement_invalidates_effectful_ancestors_not_just_cache_nodes() {
        use super::super::{Binary,BlockContext,EvalWorkspace,Node,NodeId,Program};
        let parameters=NoiseParameters::parse(&json!({"base_octave":-3,"octave_count":2,"base_amplitude":0.75})).unwrap();
        let noise=parameters.instantiate_from_random(&mut LegacyRandomSource::new(937));
        let mut program=Program {nodes:vec![Node::Noise {sampler:0,xz_scale:0.137,y_scale:0.193,shifts:[None;3]},
            Node::Interpolated {input:NodeId(0),xz:4,y:7,inverse_xz:0.25,inverse_y:1.0/7.0},
            Node::Cache {input:NodeId(1),slot:0},Node::Constant(0.0),
            Node::Binary {operation:Binary::Add,left:NodeId(2),right:NodeId(3)}],
            roots:vec![NodeId(2),NodeId(4)],effects:vec![false,false,true,false,true],cache_count:1,
            noises:vec![noise],islands:None,tables:Default::default(),scratch_frames:2,scalar_capacity:8};
        let a=SampleVolume::new(BlockContext{x:-3,y:-5,z:1},[3,5,2],[1;3]).unwrap();
        let b=SampleVolume::new(BlockContext{x:20,y:20,z:20},[3,5,2],[1;3]).unwrap();
        let p=BlockContext{x:-1,y:-1,z:2};
        let mut workspace=EvalWorkspace::new(&program);
        workspace.prepare_volume(&program,a,512).unwrap();
        let pointers=(workspace.volume_scratch.as_ptr(),workspace.cache_slots.as_ptr());
        let mut output=[0.0;30];
        program.sample_volume(a,&mut output,&mut workspace).unwrap();
        workspace.epoch=1;
        assert_eq!(program.evaluate(NodeId(4),p,&mut workspace).unwrap().to_bits(),0xbea28807);
        assert!(workspace.cache_slots[0].point.is_none());
        program.sample_volume(b,&mut output,&mut workspace).unwrap();
        workspace.epoch=1;
        assert_eq!(program.evaluate(NodeId(4),p,&mut workspace).unwrap().to_bits(),0xbea28806);

        workspace.reset_request();
        program.effects[4]=false;
        program.sample_volume(a,&mut output,&mut workspace).unwrap();
        workspace.epoch=1;
        assert_eq!(program.evaluate(NodeId(4),p,&mut workspace).unwrap().to_bits(),0xbea28807);
        program.sample_volume(b,&mut output,&mut workspace).unwrap();
        workspace.epoch=1;
        let stale=program.evaluate(NodeId(4),p,&mut workspace).unwrap().to_bits();
        assert_eq!(stale,0xbea28807);
        assert_ne!(stale,0xbea28806);
        program.effects[4]=true;

        workspace.reset_request();
        assert_eq!(program.sample(p,&mut workspace).unwrap().to_bits(),0xbea28806);
        program.sample_volume(a,&mut output,&mut workspace).unwrap();
        assert_eq!(program.sample(p,&mut workspace).unwrap().to_bits(),0xbea28806);
        assert_eq!(pointers,(workspace.volume_scratch.as_ptr(),workspace.cache_slots.as_ptr()));
    }

    #[test]
    fn zero_octaves_remain_zero_without_sampling_layers() {
        let parameters = NoiseParameters::parse(&json!({"base_octave":-2,"octave_count":2,
            "amplitude_modifiers":[0,0]})).unwrap();
        let noise = parameters.instantiate(37, Algorithm::Xoroshiro, "minecraft:test").unwrap();
        assert!(noise.layers.is_empty());
        assert_eq!(noise.sample(12.375,-27.125,63.75).to_bits(), 0);
    }

    #[test]
    fn smeared_lattice_matches_independent_scalar_and_volume_witnesses() {
        let mut noise=Perlin::new(&mut LegacyRandomSource::new(937));noise.smear=0.271;
        for (position,expected) in [([13.125,-37.25,21.0625],0x3ef2faeb),
            ([-163.75,63.125,819.5],0x3ef023fd),([0.1,0.001,-0.3],0x3d818e68)] {
            assert_eq!(noise.sample(position).to_bits(),expected,"{position:?}");
        }
        let volume=SampleVolume::new(super::super::BlockContext{x:-137,y:-59,z:811},[2,3,2],[7,2,13]).unwrap();
        let mut output=[0.0;12];noise.add_volume(volume,&mut output,0.137,0.193,0.731);
        assert_eq!(output.map(f32::to_bits),[0xbe49d792,0xbf2344c5,0xbec7c001,0xbe5f59f5,0xbe94a85b,0x3e7bccff,
            0xb79fe800,0x3e15eb9d,0xbd09ccfb,0x3ddc25ce,0x3eac2541,0x3d5d5504]);
    }

    #[test]
    fn blended_dimension_parameters_match_independent_full_sampler_witnesses() {
        use super::super::{BlockContext,BuildContext,EvalWorkspace,Program};
        let cases=[
            (937,Algorithm::Legacy,[0.25,0.375,80.0,60.0,8.0],
                [0xbe76d950,0x3df5c56c,0x3e50786d],
                [0x3e2aafd0,0x3e20ba38,0x3e5b4e55,0x3e47a5bc,0x3e4e3c88,0x3e66f3f1,
                 0x3e1d79f1,0x3e0d2cf6,0x3e10257b,0x3e33a76d,0x3e36871e,0x3e49a272]),
            (-18413,Algorithm::Legacy,[0.25,0.25,80.0,160.0,4.0],
                [0x3e2a2134,0xbc9e71dc,0x3dcad7f0],
                [0x3e2ba824,0x3e283c03,0x3e30c1ca,0x3e1fa926,0x3e1512e8,0x3e179529,
                 0x3e5ce4af,0x3e41432c,0x3e3ff5b0,0x3e4d3bbc,0x3e331b08,0x3e27b236]),
            (937,Algorithm::Xoroshiro,[0.25,0.125,80.0,160.0,8.0],
                [0xbe350f86,0xbe6fb28d,0xbd8240cc],
                [0xbe114a82,0xbdfbd282,0xbe4f5cea,0xbd636864,0xbc054f28,0xbc5f8390,
                 0x3e1100eb,0x3e0cac1f,0x3e0f03ed,0x3e102079,0x3e067381,0x3e080186]),
        ];
        for (seed,algorithm,[xz,y,xz_factor,y_factor,smear],scalar,bulk) in cases {
            let context=BuildContext {seed,algorithm,density_functions:&|_|None,noises:&|_|None};
            let document=json!({"type":"old_blended_noise","xz_scale":xz,"y_scale":y,
                "xz_factor":xz_factor,"y_factor":y_factor,"smear_scale_multiplier":smear});
            let program=Program::parse_roots_with_context(&[document.clone(),document],&context).unwrap();
            assert_eq!(program.noises.len(),3);
            assert_eq!(program.roots[0],program.roots[1]);
            let mut workspace=EvalWorkspace::new(&program);
            for ([x,y,z],expected) in [[13,-37,21],[-163,63,819],[2147483123,63,2147483440]].into_iter().zip(scalar) {
                assert_eq!(program.sample(BlockContext{x,y,z},&mut workspace).unwrap().to_bits(),expected,"{seed} {algorithm:?} {x},{y},{z}");
            }
            let volume=SampleVolume::new(BlockContext{x:-137,y:-59,z:811},[2,3,2],[7,2,13]).unwrap();
            workspace.prepare_volume(&program,volume,256).unwrap();
            let mut output=[0.0;12];
            program.sample_volume(volume,&mut output,&mut workspace).unwrap();
            assert_eq!(output.map(f32::to_bits),bulk,"{seed} {algorithm:?}");
        }
    }

    #[test]
    fn blended_ingress_rejects_invalid_parameters_and_unseeded_loading() {
        use super::super::{BuildContext,Program};
        let document=json!({"type":"old_blended_noise","xz_scale":0.25,"y_scale":0.125,
            "xz_factor":80,"y_factor":160,"smear_scale_multiplier":8});
        assert!(Program::parse(&document,&|_|None).is_err());
        let context=BuildContext {seed:0,algorithm:Algorithm::Xoroshiro,density_functions:&|_|None,noises:&|_|None};
        for (field,bad) in [("xz_scale",0.0),("y_scale",1001.0),("xz_factor",0.0),
            ("y_factor",1001.0),("smear_scale_multiplier",0.5),("smear_scale_multiplier",9.0)] {
            let mut invalid=document.clone();invalid[field]=json!(bad);
            assert!(Program::parse_with_context(&invalid,&context).is_err(),"{field}={bad}");
        }
    }
}
