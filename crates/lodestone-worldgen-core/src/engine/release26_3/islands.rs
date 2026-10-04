use crate::rng::{LegacyRandomSource,RandomSource};

use super::{BuildError,SampleVolume};

#[derive(Clone, Debug)]
pub(super) struct IslandNoise {permutation:[u8;256]}

impl IslandNoise {
    pub(super) fn new(seed:i64)->Self {
        let mut random=LegacyRandomSource::new(seed);
        random.consume_count(17_292);
        Self {permutation:super::noise::permutation(&mut random)}
    }

    fn permute(&self,index:i32)->i32 {i32::from(self.permutation[(index&255) as usize])}

    fn simplex(&self,x:f64,z:f64)->f32 {
        let skew=0.5*(3.0_f64.sqrt()-1.0);
        let unskew=(3.0-3.0_f64.sqrt())/6.0;
        let offset=(x+z)*skew;
        let ix=(x+offset).floor() as i32;
        let iz=(z+offset).floor() as i32;
        let offset=f64::from(ix.wrapping_add(iz))*unskew;
        let dx=x-(f64::from(ix)-offset);
        let dz=z-(f64::from(iz)-offset);
        let (sx,sz)=if dx>dz {(1,0)} else {(0,1)};
        let points=[(dx,dz),(dx-f64::from(sx)+unskew,dz-f64::from(sz)+unskew),
            (dx-1.0+2.0*unskew,dz-1.0+2.0*unskew)];
        let hashes=[self.permute((ix&255)+self.permute(iz&255)),
            self.permute((ix&255)+sx+self.permute((iz&255)+sz)),
            self.permute((ix&255)+1+self.permute((iz&255)+1))];
        let corners=std::array::from_fn::<_,3,_>(|index| {
            let (x,z)=points[index];
            let weight=(0.5-x*x)-z*z;
            if weight<0.0 {0.0} else {
                let weight=weight*weight;
                let gradient=super::noise::GRADIENTS[(hashes[index]%12) as usize];
                weight*weight*((f64::from(gradient[0])*x+f64::from(gradient[1])*z)+f64::from(gradient[2])*0.0)
            }
        });
        (70.0*((corners[0]+corners[1])+corners[2])) as f32
    }

    pub(super) fn sample(&self,x:i32,z:i32)->f32 {
        let x=x/8;let z=z/8;
        let base_x=x/2;let base_z=z/2;
        let remainder_x=x%2;let remainder_z=z%2;
        let mut height=-100.0_f32;
        for dx in -12..=12 {
            for dz in -12..=12 {
                let cx=i64::from(base_x.wrapping_add(dx));
                let cz=i64::from(base_z.wrapping_add(dz));
                if cx*cx+cz*cz<=4096 || !(self.simplex(cx as f64,cz as f64) < -0.9_f32) {continue;}
                let scale=((cx as f32).abs()*3439.0+(cz as f32).abs()*147.0)%13.0+9.0;
                let x=(remainder_x-dx*2) as f32;
                let z=(remainder_z-dz*2) as f32;
                let distance=f64::from(x*x+z*z).sqrt() as f32;
                height=super::java_max(height,super::java_clamp(100.0-distance*scale,-100.0,80.0));
            }
        }
        (height-8.0)/128.0
    }

    pub(super) fn fill(&self,volume:SampleVolume,output:&mut[f32])->Result<(),BuildError> {
        volume.check_output(output)?;
        for z in 0..volume.size[2] {
            for x in 0..volume.size[0] {
                let value=self.sample(volume.coordinate(0,x),volume.coordinate(2,z));
                let index=volume.index([x,0,z]);
                output[index..index+volume.size[1]].fill(value);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{BlockContext,BuildContext,EvalWorkspace,Program};
    use crate::rng::Algorithm;
    use serde_json::json;

    #[test]
    fn island_initialization_and_heights_match_independent_mixed_precision_witnesses() {
        let positions=[(0,0),(1024,-1024),(-1193,2177),(8173,3191),(-2145,-1833),(1000001,999997)];
        let cases=[
            (0,[9,165,129,139,252,242,221,76,188,103,25,205,168,148,130,254],
                [0xbf580000,0xbefb5e10,0x3e48b784,0xbe0abf7c,0x3e9897ea,0x3dc00000]),
            (937,[217,158,27,199,247,181,39,144,248,57,218,126,166,129,60,139],
                [0xbf580000,0xbf2dc558,0xbe77deb0,0xbeeb7850,0xbf580000,0xbf18db26]),
            (-18413,[79,23,237,53,195,81,9,84,151,11,35,112,43,106,216,241],
                [0xbf580000,0xbf3fa8d0,0x3edfc728,0xbeb03ffc,0xbdf54cd8,0xbe06d32c]),
        ];
        for (seed,prefix,expected) in cases {
            let noise=IslandNoise::new(seed);
            assert_eq!(noise.permutation[..16],prefix,"seed {seed}");
            for ((x,z),expected) in positions.into_iter().zip(expected) {
                assert_eq!(noise.sample(x,z).to_bits(),expected,"seed {seed} at {x},{z}");
            }
        }
    }

    #[test]
    fn island_program_shares_one_sampler_and_broadcasts_y_for_both_algorithms() {
        let document=json!({"type":"end_outer_islands"});
        assert!(Program::parse(&document,&|_|None).is_err());
        for algorithm in [Algorithm::Legacy,Algorithm::Xoroshiro] {
            let context=BuildContext {seed:0,algorithm,density_functions:&|_|None,noises:&|_|None};
            let program=Program::parse_roots_with_context(&[document.clone(),document.clone()],&context).unwrap();
            assert_eq!(program.roots[0],program.roots[1]);
            let mut workspace=EvalWorkspace::new(&program);
            let volume=SampleVolume::new(BlockContext{x:-1193,y:37,z:2177},[2,2,2],[17,13,19]).unwrap();
            workspace.prepare_volume(&program,volume,32).unwrap();
            let mut output=[0.0;8];
            program.sample_volume(volume,&mut output,&mut workspace).unwrap();
            assert_eq!(output.map(f32::to_bits),[0x3e48b784,0x3e48b784,0x3e11ad94,0x3e11ad94,
                0x3eba9557,0x3eba9557,0x3e940000,0x3e940000]);
        }
    }
}
