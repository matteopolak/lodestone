//! Release entrypoint for the finite heavyweight server profiling run.

use lodestone_server::heavy_scene::{
    HeavyScenePlan, HeavyServerArgs, HeavyServerHarness, emit_scene,
};
use lodestone_v26_2::V770ServerProtocol;

#[tokio::main]
async fn main() -> Result<(), lodestone_server::heavy_scene::HeavyError> {
    let args = HeavyServerArgs::parse_env()?;
    let plan: HeavyScenePlan = args.spec.build_plan()?;
    if let Some(path) = args.emit_scene.as_deref() {
        return emit_scene(&plan, path);
    }
    let record = HeavyServerHarness::run(args, plan, V770ServerProtocol).await?;
    println!("{}", serde_json::to_string(&record)?);
    Ok(())
}
