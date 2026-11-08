//! IERS finals2000A parser (fixed-width and CSV) and interpolation.
#![no_main]

use chronus_gateway::freshness::parse_finals;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: (&str, i64)| {
    let (text, t) = input;
    let Ok(table) = parse_finals(text) else {
        return;
    };
    assert!(!table.is_empty());
    if let Some(at) = chrono::DateTime::from_timestamp(t % 10_000_000_000, 0)
        && let Some(v) = table.at(at)
    {
        assert!(v.ut1_minus_utc_s.abs() <= 1.0);
        assert!(v.x_arcsec.abs() <= 1.0 && v.y_arcsec.abs() <= 1.0);
    }
});
