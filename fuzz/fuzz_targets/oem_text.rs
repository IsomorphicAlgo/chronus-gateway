//! CCSDS OEM (KVN) parser, then interpolation at arbitrary instants inside the coverage.
#![no_main]

use chronus_gateway::oem::OemEphemeris;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: (&str, f64)| {
    let (text, frac) = input;
    let Ok(oem) = OemEphemeris::parse(text) else {
        return;
    };
    let (start, stop) = oem.coverage();
    assert!(start <= stop);
    if frac.is_finite() {
        let span = (stop - start).num_milliseconds() as f64;
        let t = start + chrono::Duration::milliseconds((span * frac.clamp(0.0, 1.0)) as i64);
        let _ = oem.state_itrf(t, &ephemerust::EarthOrientation::default());
    }
});
