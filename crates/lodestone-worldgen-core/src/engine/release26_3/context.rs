use std::sync::Arc;

use super::{BlockContext,BuildError,EvalState,EvalWorkspace,SampleVolume};

/// Typed request inputs. Blending inputs are uniform along Y.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextInput { BlendAlpha, BlendOffset, StructureDensity }

/// Immutable world inputs bound once for a generation request. Implementations
/// must retain distinct scalar and whole-volume sampling contracts.
pub trait RequestContext: std::fmt::Debug + Send + Sync {
    fn sample(&self, input:ContextInput, position:BlockContext)->Result<f32,BuildError>;
    fn sample_volume(&self, input:ContextInput, volume:SampleVolume, output:&mut[f32])->Result<(),BuildError>;
    fn blend_density(&self, position:BlockContext, density:f32)->Result<f32,BuildError>;

    fn blend_density_volume(&self,volume:SampleVolume,output:&mut[f32])->Result<(),BuildError> {
        volume.check_output(output)?;
        for (index,value) in output.iter_mut().enumerate() {
            let [x,y,z]=volume.indices(index);
            *value=self.blend_density(BlockContext {x:volume.coordinate(0,x),y:volume.coordinate(1,y),z:volume.coordinate(2,z)},*value)?;
        }
        Ok(())
    }
}

/// Explicitly selects a request with no structures or neighboring blend data.
#[derive(Clone, Copy, Debug, Default)]
pub struct EmptyContext;

impl RequestContext for EmptyContext {
    fn sample(&self,input:ContextInput,_:BlockContext)->Result<f32,BuildError> {
        Ok(if input==ContextInput::BlendAlpha {1.0} else {0.0})
    }
    fn sample_volume(&self,input:ContextInput,volume:SampleVolume,output:&mut[f32])->Result<(),BuildError> {
        volume.check_output(output)?;
        output.fill(if input==ContextInput::BlendAlpha {1.0} else {0.0});
        Ok(())
    }
    fn blend_density(&self,_:BlockContext,density:f32)->Result<f32,BuildError> {Ok(density)}
    fn blend_density_volume(&self,volume:SampleVolume,output:&mut[f32])->Result<(),BuildError> {volume.check_output(output)}
}

impl EvalWorkspace {
    /// Starts a new context boundary while retaining prepared request storage.
    pub fn bind_context(&mut self,context:Arc<dyn RequestContext>) {
        self.reset_request();
        self.request_context=Some(context);
    }
}

impl EvalState<'_> {
    pub(super) fn request_context(&self)->Result<&dyn RequestContext,BuildError> {
        self.request_context.ok_or_else(||BuildError::new("context","density graph requires an explicit request context"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::Program;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize,Ordering};

    #[derive(Debug,Default)]
    struct Fixture {scalar:AtomicUsize,bulk:AtomicUsize,transforms:AtomicUsize}

    impl Fixture {
        fn value(input:ContextInput,p:BlockContext)->f32 {
            match input {
                ContextInput::BlendAlpha => (p.x+p.y+2) as f32,
                ContextInput::BlendOffset => (p.z+p.y+3) as f32,
                ContextInput::StructureDensity => (p.y-4) as f32,
            }
        }
    }

    impl RequestContext for Fixture {
        fn sample(&self,input:ContextInput,p:BlockContext)->Result<f32,BuildError> {
            self.scalar.fetch_add(1,Ordering::Relaxed);
            Ok(Self::value(input,p))
        }
        fn sample_volume(&self,input:ContextInput,volume:SampleVolume,output:&mut[f32])->Result<(),BuildError> {
            volume.check_output(output)?;
            self.bulk.fetch_add(1,Ordering::Relaxed);
            for (index,out) in output.iter_mut().enumerate() {
                let [x,y,z]=volume.indices(index);
                *out=100.0+Self::value(input,BlockContext {x:volume.coordinate(0,x),y:volume.coordinate(1,y),z:volume.coordinate(2,z)});
            }
            Ok(())
        }
        fn blend_density(&self,p:BlockContext,density:f32)->Result<f32,BuildError> {
            self.transforms.fetch_add(1,Ordering::Relaxed);
            Ok(density+p.y as f32*0.25)
        }
    }

    #[test]
    fn context_binding_is_explicit_and_empty_values_are_exact() {
        for (ty,expected) in [("blend_alpha",1.0),("blend_offset",0.0),("beardifier",0.0)] {
            let program=Program::parse(&json!({"type":ty}),&|_|None).unwrap();
            let mut workspace=EvalWorkspace::new(&program);
            assert!(program.sample(BlockContext::default(),&mut workspace).unwrap_err().reason.contains("explicit request context"));
            workspace.bind_context(Arc::new(EmptyContext));
            assert_eq!(program.sample(BlockContext::default(),&mut workspace).unwrap(),expected);
        }
        let program=Program::parse(&json!({"type":"blend_density","input":-0.0}),&|_|None).unwrap();
        let mut workspace=EvalWorkspace::new(&program);
        workspace.bind_context(Arc::new(EmptyContext));
        assert_eq!(program.sample(BlockContext{x:9,y:13,z:17},&mut workspace).unwrap().to_bits(),0x8000_0000);
    }

    #[test]
    fn context_axes_and_bulk_dispatch_preserve_distinct_contracts() {
        let program=Program::parse(&json!({"type":"blend_density","input":{"type":"add",
            "left":{"type":"blend_alpha"},"right":{"type":"beardifier"}}}),&|_|None).unwrap();
        let mut workspace=EvalWorkspace::new(&program);
        let fixture=Arc::new(Fixture::default());
        workspace.bind_context(fixture.clone());
        let position=BlockContext{x:2,y:7,z:3};
        assert_eq!(program.sample(position,&mut workspace).unwrap(),8.75);
        let volume=SampleVolume::new(position,[2,2,1],[3,5,1]).unwrap();
        workspace.prepare_volume(&program,volume,128).unwrap();
        let mut output=[0.0;4];
        program.sample_volume(volume,&mut output,&mut workspace).unwrap();
        assert_eq!(output,[208.75,215.0,211.75,218.0]);
        assert_eq!(fixture.scalar.load(Ordering::Relaxed),2);
        assert_eq!(fixture.bulk.load(Ordering::Relaxed),2);
        assert_eq!(fixture.transforms.load(Ordering::Relaxed),5);
    }

    #[test]
    fn rebinding_context_invalidates_explicit_cache_without_losing_storage() {
        let program=Program::parse(&json!({"type":"cache","input":{"type":"beardifier"}}),&|_|None).unwrap();
        let mut workspace=EvalWorkspace::new(&program);
        let volume=SampleVolume::new(BlockContext{x:2,y:7,z:3},[2,1,1],[1;3]).unwrap();
        workspace.prepare_volume(&program,volume,64).unwrap();
        workspace.bind_context(Arc::new(Fixture::default()));
        let mut output=[0.0;2];
        program.sample_volume(volume,&mut output,&mut workspace).unwrap();
        assert_eq!(output,[103.0;2]);
        workspace.bind_context(Arc::new(EmptyContext));
        assert_eq!(program.sample(volume.min(),&mut workspace).unwrap(),0.0);
        program.sample_volume(volume,&mut output,&mut workspace).unwrap();
        assert_eq!(output,[0.0;2]);
    }

    #[derive(Debug,Default)]
    struct Counter(AtomicUsize);
    impl RequestContext for Counter {
        fn sample(&self,_:ContextInput,_:BlockContext)->Result<f32,BuildError> {
            Ok((self.0.fetch_add(1,Ordering::Relaxed)+1) as f32)
        }
        fn sample_volume(&self,_:ContextInput,_:SampleVolume,_:&mut[f32])->Result<(),BuildError> {unreachable!()}
        fn blend_density(&self,_:BlockContext,density:f32)->Result<f32,BuildError> {Ok(density)}
    }

    #[test]
    fn context_sensitive_ancestors_are_not_common_subexpression_memoized() {
        let branch=json!({"type":"add","left":{"type":"beardifier"},"right":1});
        let program=Program::parse(&json!({"type":"add","left":branch,"right":branch}),&|_|None).unwrap();
        let mut workspace=EvalWorkspace::new(&program);
        workspace.bind_context(Arc::new(Counter::default()));
        assert_eq!(program.sample(BlockContext::default(),&mut workspace).unwrap(),5.0);
        let mut wrong=program.clone();wrong.effects.fill(false);
        workspace.bind_context(Arc::new(Counter::default()));
        assert_eq!(wrong.sample(BlockContext::default(),&mut workspace).unwrap(),4.0);
    }
}
