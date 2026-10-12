//! CCSDS OEM backend (Methodology D-045) against independent references.
//!
//! The fixtures in `tests/data/` are one 3-hour ISS ephemeris (TLE propagated with python-sgp4)
//! written four ways — GCRF/UTC, EME2000/TAI, TEME/GPS and ITRF2014/TT with day-of-year epochs —
//! and `iss_itrf_truth.csv` holds astropy/ERFA ITRS states at times *between* the 60 s samples.
//! Each file must reproduce the truth, exercising the parser, every supported frame and time
//! system, Lagrange interpolation, and Ephemerust's Earth-orientation frame chain together.

use std::path::Path;

use chrono::{Duration, TimeZone, Utc};
use chronus_gateway::config::{EphemerisSource, StationConfig};
use chronus_gateway::oem::{OemEphemeris, OemError, OemPropagator};
use chronus_gateway::propagator::{
    EphemerustPropagator, OrbitalPropagator, StationModel, TrackingError, propagator_from_station,
};
use ephemerust::EarthOrientation;

const FILES: [&str; 4] = [
    "iss_gcrf_utc.oem",
    "iss_eme2000_tai.oem",
    "iss_teme_gps.oem",
    "iss_itrf_tt_doy.oem",
];

fn data(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

/// Earth orientation astropy used for the fixtures (header of `iss_itrf_truth.csv`).
fn fixture_eop() -> EarthOrientation {
    EarthOrientation::new(-0.2273171, 0.1875301, 0.4170862).unwrap()
}

fn t0() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2020, 7, 12, 21, 0, 0).unwrap()
}

fn truth() -> Vec<(f64, [f64; 3], [f64; 3])> {
    std::fs::read_to_string(data("iss_itrf_truth.csv"))
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| {
            let f: Vec<f64> = l.split(',').map(|x| x.parse().unwrap()).collect();
            (f[0], [f[1], f[2], f[3]], [f[4], f[5], f[6]])
        })
        .collect()
}

fn dist(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

#[test]
fn every_frame_and_time_system_reproduces_astropy_itrf() {
    let eop = fixture_eop();
    let truth = truth();
    assert_eq!(truth.len(), 23);
    for name in FILES {
        let eph = OemEphemeris::from_file(&data(name)).unwrap();
        let (mut worst_r_m, mut worst_v_mm_s) = (0.0_f64, 0.0_f64);
        for (dt, r_ref, v_ref) in &truth {
            let t = t0() + Duration::milliseconds((dt * 1000.0) as i64);
            let (r, v) = eph.state_itrf(t, &eop).unwrap();
            worst_r_m = worst_r_m.max(dist(r, *r_ref) * 1000.0);
            worst_v_mm_s = worst_v_mm_s.max(dist(v, *v_ref) * 1e6);
        }
        eprintln!("{name}: worst {worst_r_m:.4} m, {worst_v_mm_s:.4} mm/s");
        // Measured: ~5.5 cm / 0.07 mm/s for the inertial files (astropy interpolates UT1-UTC
        // across the 3 h while the gateway holds one day's value), 1 mm for ITRF (interpolation only).
        assert!(worst_r_m < 0.15, "{name}: position error {worst_r_m:.4} m");
        assert!(
            worst_v_mm_s < 0.5,
            "{name}: velocity error {worst_v_mm_s:.4} mm/s"
        );
    }
}

#[test]
fn oem_backend_tracks_the_same_orbit_as_the_tle_backend() {
    let station = StationModel {
        earth_orientation: fixture_eop(),
        ..StationModel::geometric(47.9088, -122.2503, 0.0)
    };
    let oem = OemPropagator::new(
        OemEphemeris::from_file(&data("iss_gcrf_utc.oem")).unwrap(),
        station,
    );
    let tle = EphemerustPropagator::new(
        chronus_gateway::config::DEFAULT_ISS_TLE,
        47.9088,
        -122.2503,
        0.0,
    )
    .unwrap()
    .with_station_model(station);
    let (mut d_az, mut d_rng, mut d_rr) = (0.0_f64, 0.0_f64, 0.0_f64);
    for k in 1..170 {
        let t = t0() + Duration::seconds(60 * k + 17);
        let a = oem.tracking_state(t).unwrap();
        let b = tle.tracking_state(t).unwrap();
        d_az = d_az.max((a.elevation_deg - b.elevation_deg).abs());
        d_rng = d_rng.max((a.range_km - b.range_km).abs());
        d_rr = d_rr.max((a.range_rate_km_s - b.range_rate_km_s).abs());
        #[cfg(feature = "hil")]
        assert!(a.nadir_sun_illum_cos.is_finite());
    }
    eprintln!(
        "OEM vs TLE backend: {d_az:.5} deg el, {:.1} m range, {:.4} m/s",
        d_rng * 1e3,
        d_rr * 1e3
    );
    // The fixture used WGS72 SGP4 constants and the gateway's TLE backend uses WGS84: tens of
    // metres apart after hours, far inside any physics tolerance.
    assert!(d_az < 0.01 && d_rng < 0.2 && d_rr < 0.001);
}

#[test]
fn station_config_selects_the_oem_backend_and_refuses_extrapolation() {
    let cfg = {
        let mut c = StationConfig::default();
        c.site.latitude_deg = 47.9088;
        c.site.longitude_deg = -122.2503;
        c.site.altitude_m = 0.0;
        c.tracking.ephemeris = EphemerisSource::OemFile(data("iss_eme2000_tai.oem"));
        c
    };
    let prop = propagator_from_station(&cfg).expect("OEM backend from config");
    let s = prop.tracking_state(t0() + Duration::minutes(30)).unwrap();
    // Refraction is on by default, so the apparent elevation is reported and differs.
    assert!(s.apparent_elevation_deg.is_finite());
    assert!(s.apparent_elevation_deg >= s.elevation_deg);

    let err = prop.tracking_state(t0() - Duration::hours(1)).unwrap_err();
    assert!(matches!(
        err,
        TrackingError::Oem(OemError::OutOfCoverage { .. })
    ));
    let d = chronus_gateway::diagnose::explain_tracking_error(&err);
    assert_eq!(d.code, "CG-322");
}

#[test]
fn missing_oem_file_is_explained() {
    let cfg = {
        let mut c = StationConfig::default();
        c.tracking.ephemeris = EphemerisSource::OemFile("no/such/ephemeris.oem".into());
        c
    };
    let err = propagator_from_station(&cfg).err().expect("must fail");
    assert_eq!(
        chronus_gateway::diagnose::explain_tracking_error(&err).code,
        "CG-127"
    );
}
