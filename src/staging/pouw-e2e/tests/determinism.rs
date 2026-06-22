use commputer_pouw_e2e::{programs, world::JobWorld};
use commputer_pouw::wasm::{ProgramStore, WasmLimits, WasmOracle};
use commputer_da::params::ChunkingParams;

#[test]
fn scenario_8_two_independent_oracles_agree_on_da_bytes() {
    let world = JobWorld::new(
        programs::assemble(programs::DOUBLER),
        programs::DEFAULT_INPUT.to_vec(),
        ChunkingParams::default(),
    );
    let att = world.publish();

    // Two INDEPENDENT DA reconstructions (separate gate passes), each feeding a fresh engine +
    // store — proving both that DA reconstruction is reproducible AND that two independent wasmi
    // engines agree on result + fuel (spec §5.1 / §9 scenario 8).
    let bytes_a = world.gate_pool(&att).program_bytes.expect("available");
    let bytes_b = world.gate_pool(&att).program_bytes.expect("available");
    assert_eq!(bytes_a, bytes_b, "DA reconstruction is reproducible across independent gate passes");
    let mut sa = ProgramStore::new(); sa.insert(bytes_a);
    let mut sb = ProgramStore::new(); sb.insert(bytes_b);
    let oa = WasmOracle::new(sa, WasmLimits::default());
    let ob = WasmOracle::new(sb, WasmLimits::default());

    let spec = world.job_spec(&att);
    let ra = oa.execute(&spec, &world.input);
    let rb = ob.execute(&spec, &world.input);
    assert_eq!(ra.result, rb.result, "identical outcome on DA-delivered bytes");
    assert_eq!(ra.fuel_consumed, rb.fuel_consumed, "fuel is consensus-equal (spec §6)");
}
