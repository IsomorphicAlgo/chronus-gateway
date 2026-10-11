//! One visitor's private simulated spacecraft + gateway state.
//!
//! [`SessionSim`] owns its own [`SharedGateway`] (metrics, propagator, throttle cache) and its own
//! **simulated clock**, so sessions never share mutable state. The clock runs `speed`× faster than
//! wall time (default 60×: one ISS orbit ≈ 92 s) so eclipses and passes are visible in a short demo.
//!
//! Frames are stamped with the *simulated* time as `received_at`, so the gateway evaluates physics
//! at exactly the instant the synthetic spacecraft "transmitted" — nominal telemetry is therefore
//! self-consistent and raises no alarms; each [`Scenario`] perturbs exactly one thing.

use std::net::SocketAddr;
use std::sync::Arc;

use chrono::{DateTime, Duration, TimeZone, Utc};
use chronus_gateway::config::StationConfig;
use chronus_gateway::http::process_frame_with_rf;
use chronus_gateway::ingest::{IngestStats, RawFrame};
use chronus_gateway::metrics::GatewayMetrics;
use chronus_gateway::propagator::{EphemerustPropagator, TrackingProvider};
use chronus_gateway::state::SharedGateway;
use chronus_gateway::validate::{RfMetadata, expected_carrier_hz, expected_rx_power_dbm};
use chronus_gateway::{encode_hil_tm_v1_payload, encode_synthetic_tm};
use chronus_hil_sim::{
    HilScriptedAnomaly, HilScriptedAnomalyKind, TelemSample, apply_scripted_anomaly,
    hil_targets_from_illum,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::broadcast;

/// CCSDS APID inside the gateway's default HIL TM v1 band (`0x7B0..=0x7BF`).
pub const SANDBOX_APID: u16 = 0x7B0;
/// Simulated start instant: 15 minutes before an overhead (max elevation ≈ 90°) ISS pass over the
/// default station, so a visitor sees the spacecraft rise within seconds at the default 60×.
/// Found by scanning the bundled public ISS TLE (epoch 2020-07-12); it is deterministic.
#[must_use]
pub fn sandbox_start_utc() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2020, 7, 13, 4, 50, 0)
        .single()
        .expect("static start instant is a valid UTC time")
}

/// Lowest / highest accepted time-acceleration factor.
pub const SPEED_RANGE: (f64, f64) = (1.0, 300.0);
/// Default time acceleration.
pub const DEFAULT_SPEED: f64 = 60.0;
/// Horizon mask used when the visitor has not asked for one (i.e. effectively "off").
pub const MASK_OFF_DEG: f64 = -90.0;
/// Horizon mask applied when the visitor enables it.
pub const MASK_ON_DEG: f64 = 0.0;

/// A single, labelled thing a visitor can break.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scenario {
    /// Everything consistent with physics.
    Nominal,
    /// Reported bus voltage far from the illumination model (bit 4).
    EpsVoltage,
    /// Reported panel temperature far from the illumination model (bit 5).
    Thermal,
    /// Reported body rate above the ceiling (bit 6).
    BodyRate,
    /// Reports full-sun power/temperature even while in shadow — the README "shadow check" (bits 4/5).
    FullPowerClaim,
    /// Measured carrier offset from the Doppler-predicted value (bit 0).
    DopplerSpoof,
    /// Measured antenna direction offset from the computed look angle (bit 3).
    PointingError,
    /// Received power far below the free-space link budget (bit 2).
    LowSignal,
}

/// Static description of a [`Scenario`] for the UI.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ScenarioInfo {
    pub id: Scenario,
    pub label: &'static str,
    pub description: &'static str,
    /// `physics_flags` bit numbers this scenario is expected to raise (empty = none).
    pub expected_bits: &'static [u8],
}

/// All scenarios, in UI order.
pub const SCENARIOS: &[ScenarioInfo] = &[
    ScenarioInfo {
        id: Scenario::Nominal,
        label: "Nominal",
        description: "Telemetry consistent with orbital physics. No alarms expected.",
        expected_bits: &[],
    },
    ScenarioInfo {
        id: Scenario::FullPowerClaim,
        label: "Claims full power in shadow",
        description: "The spacecraft reports full-sun power and temperature even when the propagator says it is in Earth's shadow.",
        expected_bits: &[4, 5],
    },
    ScenarioInfo {
        id: Scenario::DopplerSpoof,
        label: "Doppler spoof",
        description: "The measured carrier is 2 kHz away from what the live range-rate predicts, as a replayed or spoofed signal would be.",
        expected_bits: &[0],
    },
    ScenarioInfo {
        id: Scenario::PointingError,
        label: "Antenna pointing error",
        description: "The antenna reports looking 2 degrees away from where the spacecraft actually is.",
        expected_bits: &[3],
    },
    ScenarioInfo {
        id: Scenario::LowSignal,
        label: "Signal 12 dB too weak",
        description: "Received power is far below the free-space link budget for the current slant range.",
        expected_bits: &[2],
    },
    ScenarioInfo {
        id: Scenario::EpsVoltage,
        label: "Bus voltage fault",
        description: "Reported EPS bus voltage is stuck at 10 V regardless of illumination.",
        expected_bits: &[4],
    },
    ScenarioInfo {
        id: Scenario::Thermal,
        label: "Panel overheating",
        description: "Reported panel temperature jumps to 80 C, far above the illumination-driven model.",
        expected_bits: &[5],
    },
    ScenarioInfo {
        id: Scenario::BodyRate,
        label: "Tumbling",
        description: "Reported body rate is 99 deg/s, far above the attitude ceiling.",
        expected_bits: &[6],
    },
];

/// Failure while advancing a session (the caller ends the session; nothing is retried).
#[derive(Debug)]
pub struct SimError(pub String);

impl std::fmt::Display for SimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SimError {}

/// Output of one simulation step: the gateway's own Open MCT frame plus a sandbox "tick" message
/// carrying the reported-vs-expected values for the UI.
#[derive(Debug)]
pub struct StepOutput {
    /// `openmct.realtime.v1` JSON, byte-for-byte what the real gateway would emit.
    pub frame_json: String,
    /// `chronus.sandbox.v1` tick JSON (extra context; not part of the gateway contract).
    pub tick_json: String,
    /// `physics_flags` from the frame (convenient for tests/logging).
    pub physics_flags: u8,
}

/// Small deterministic noise source (no `rand` dependency; reproducible demos and tests).
struct Noise(u64);

impl Noise {
    /// Uniform in `[-1, 1]`.
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 52) as f64 - 1.0
    }
}

/// A visitor's private simulation. Cheap to construct (one TLE parse).
pub struct SessionSim {
    state: SharedGateway,
    tracking: Arc<TrackingProvider>,
    station: Arc<StationConfig>,
    seq: u32,
    start: DateTime<Utc>,
    sim_time: DateTime<Utc>,
    speed: f64,
    scenario: Scenario,
    mask_on: bool,
    noise: Noise,
}

fn build_station(mask_on: bool) -> StationConfig {
    {
        let mut c = StationConfig::default();
        c.tolerances.minimum_elevation_deg = if mask_on { MASK_ON_DEG } else { MASK_OFF_DEG };
        c
    }
}

fn build_gateway(
    station: &Arc<StationConfig>,
) -> Result<(SharedGateway, Arc<TrackingProvider>), SimError> {
    let prop = EphemerustPropagator::from_station(station.as_ref())
        .map_err(|e| SimError(format!("propagator init: {e}")))?;
    let tracking = Arc::new(TrackingProvider::new(
        Arc::new(prop),
        station.tracking.min_recompute_interval_ms,
    ));
    let (frame_tx, _rx) = broadcast::channel(1);
    let state = SharedGateway::new(
        frame_tx,
        Arc::new(IngestStats::default()),
        Arc::new(GatewayMetrics::default()),
        Arc::clone(station),
        Some(Arc::clone(&tracking)),
    );
    Ok((state, tracking))
}

impl SessionSim {
    /// New session at the default start instant, nominal scenario, horizon mask off.
    pub fn new(noise_seed: u64) -> Result<Self, SimError> {
        let station = Arc::new(build_station(false));
        let (state, tracking) = build_gateway(&station)?;
        let start = sandbox_start_utc();
        Ok(Self {
            state,
            tracking,
            station,
            seq: 0,
            start,
            sim_time: start,
            speed: DEFAULT_SPEED,
            scenario: Scenario::Nominal,
            mask_on: false,
            noise: Noise(noise_seed | 1),
        })
    }

    pub fn scenario(&self) -> Scenario {
        self.scenario
    }

    pub fn speed(&self) -> f64 {
        self.speed
    }

    pub fn mask_on(&self) -> bool {
        self.mask_on
    }

    pub fn sim_time(&self) -> DateTime<Utc> {
        self.sim_time
    }

    pub fn set_scenario(&mut self, s: Scenario) {
        self.scenario = s;
    }

    /// Clamps to [`SPEED_RANGE`]; non-finite input is ignored.
    pub fn set_speed(&mut self, speed: f64) {
        if speed.is_finite() {
            self.speed = speed.clamp(SPEED_RANGE.0, SPEED_RANGE.1);
        }
    }

    /// Enables/disables the 0° horizon mask (rebuilds this session's private gateway state).
    pub fn set_horizon_mask(&mut self, on: bool) -> Result<(), SimError> {
        if on == self.mask_on {
            return Ok(());
        }
        let station = Arc::new(build_station(on));
        let (state, tracking) = build_gateway(&station)?;
        self.state = state;
        self.tracking = tracking;
        self.station = station;
        self.mask_on = on;
        Ok(())
    }

    /// Back to frame 0 at the start instant, nominal scenario (speed and mask are kept).
    pub fn reset(&mut self) {
        self.seq = 0;
        self.sim_time = self.start;
        self.scenario = Scenario::Nominal;
    }

    /// Advances the simulated clock by `real_dt_s × speed` and produces one frame.
    pub fn step(&mut self, real_dt_s: f64) -> Result<StepOutput, SimError> {
        let dt = real_dt_s.clamp(0.0, 1.0) * self.speed;
        self.sim_time += Duration::microseconds((dt * 1e6) as i64);

        let ts = self
            .tracking
            .tracking_state(self.sim_time)
            .map_err(|e| SimError(format!("propagation failed: {e:#}")))?;
        let st = self.station.as_ref();

        // What a healthy spacecraft would report right now.
        let illum = if ts.nadir_sun_illum_cos.is_finite() {
            ts.nadir_sun_illum_cos
        } else {
            0.0
        };
        let (exp_v, exp_t) = hil_targets_from_illum(illum);
        let seq = self.seq;
        let mut sample = TelemSample {
            seq,
            eps_bus_voltage_v: exp_v,
            thermal_panel_c: exp_t,
            body_rate_deg_s: 0.15 * (seq as f32 * 0.07).sin(),
        };

        // What the ground receive chain measures (healthy: truth plus small noise).
        let exp_carrier = expected_carrier_hz(st.site.nominal_carrier_hz, ts.range_rate_km_s);
        let exp_rx = expected_rx_power_dbm(
            ts.range_km,
            st.site.nominal_carrier_hz,
            st.tolerances.tx_power_dbm,
            st.tolerances.tx_gain_dbi,
            st.tolerances.rx_gain_dbi,
        );
        let mut carrier = exp_carrier + 30.0 * self.noise.next();
        let mut rx_power = exp_rx.map(|p| p + 0.8 * self.noise.next());
        // A real antenna tracks the *apparent* (refracted) direction (D-042).
        let pointing_el = ts.pointing_elevation_deg();
        let mut az = ts.azimuth_deg + 0.05 * self.noise.next();
        let mut el = pointing_el + 0.05 * self.noise.next();

        match self.scenario {
            Scenario::Nominal => {}
            Scenario::EpsVoltage => {
                apply_fault(&mut sample, HilScriptedAnomalyKind::EpsVoltage);
            }
            Scenario::Thermal => {
                apply_fault(&mut sample, HilScriptedAnomalyKind::Thermal);
            }
            Scenario::BodyRate => {
                apply_fault(&mut sample, HilScriptedAnomalyKind::BodyRate);
            }
            Scenario::FullPowerClaim => {
                let (v, t) = hil_targets_from_illum(1.0);
                sample.eps_bus_voltage_v = v;
                sample.thermal_panel_c = t;
            }
            Scenario::DopplerSpoof => carrier += 2_000.0,
            Scenario::PointingError => {
                az = (az + 2.0).rem_euclid(360.0);
                el = (el + 2.0).min(90.0);
            }
            Scenario::LowSignal => rx_power = rx_power.map(|p| p - 12.0),
        }

        let payload = encode_hil_tm_v1_payload(
            sample.seq,
            sample.eps_bus_voltage_v,
            sample.thermal_panel_c,
            sample.body_rate_deg_s,
        );
        let packet = encode_synthetic_tm(SANDBOX_APID, (seq & 0x3FFF) as u16, &payload);
        let raw = RawFrame {
            bytes: Arc::from(packet.as_slice()),
            received_at: self.sim_time,
            source: SocketAddr::from(([127, 0, 0, 1], 0)),
        };
        let mut rf = RfMetadata::new()
            .with_carrier_hz(carrier)
            .with_pointing_deg(az, el);
        rf.measured_rx_power_dbm = rx_power;
        let frame_json = process_frame_with_rf(&self.state, &raw, rf)
            .ok_or_else(|| SimError("gateway rejected its own synthetic frame".into()))?;
        let physics_flags = serde_json::from_str::<serde_json::Value>(&frame_json)
            .ok()
            .and_then(|v| v["physics_flags"].as_u64())
            .unwrap_or(0) as u8;

        let tick = json!({
            "chronus_schema": "chronus.sandbox.v1",
            "kind": "tick",
            "seq": seq,
            "sim_time": self.sim_time,
            "scenario": self.scenario,
            "speed": self.speed,
            "horizon_mask_deg": st.tolerances.minimum_elevation_deg,
            "reported": {
                "eps_bus_voltage_v": sample.eps_bus_voltage_v,
                "thermal_panel_c": sample.thermal_panel_c,
                "body_rate_deg_s": sample.body_rate_deg_s,
            },
            "expected": {
                "eps_bus_voltage_v": exp_v,
                "thermal_panel_c": exp_t,
                "sun_illumination": illum,
                "carrier_hz": exp_carrier,
                "rx_power_dbm": exp_rx,
                "azimuth_deg": ts.azimuth_deg,
                "elevation_deg": pointing_el,
                "geometric_elevation_deg": ts.elevation_deg,
            },
            "measured": {
                "carrier_hz": carrier,
                "rx_power_dbm": rx_power,
                "azimuth_deg": az,
                "elevation_deg": el,
            },
        });

        self.seq = self.seq.wrapping_add(1);
        Ok(StepOutput {
            frame_json,
            tick_json: tick.to_string(),
            physics_flags,
        })
    }
}

fn apply_fault(sample: &mut TelemSample, kind: HilScriptedAnomalyKind) {
    let script = HilScriptedAnomaly {
        kind,
        start_frame: 0,
        duration_frames: u32::MAX,
    };
    apply_scripted_anomaly(sample, sample.seq, &script);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Steps `n` ticks at 10 Hz wall rate and ORs together all `physics_flags`.
    fn run(sim: &mut SessionSim, n: usize) -> u8 {
        let mut seen = 0u8;
        for _ in 0..n {
            seen |= sim.step(0.1).expect("step").physics_flags;
        }
        seen
    }

    // 60x speed, 0.1 s ticks -> 6 s of sim time per tick; 1000 ticks ~ 1.1 orbits (>= one eclipse).
    const ORBIT_TICKS: usize = 1000;

    #[test]
    fn nominal_raises_no_alarms_over_an_orbit() {
        let mut sim = SessionSim::new(42).unwrap();
        assert_eq!(run(&mut sim, ORBIT_TICKS), 0);
    }

    #[test]
    fn each_fault_scenario_raises_its_documented_bits() {
        for info in SCENARIOS.iter().filter(|i| i.id != Scenario::Nominal) {
            let mut sim = SessionSim::new(7).unwrap();
            sim.set_scenario(info.id);
            let seen = run(&mut sim, ORBIT_TICKS);
            for bit in info.expected_bits {
                assert!(
                    seen & (1 << bit) != 0,
                    "{:?} should raise bit {bit}, saw {seen:#010b}",
                    info.id
                );
            }
        }
    }

    #[test]
    fn faults_are_isolated_to_their_own_bits() {
        // A pointing error must not look like a Doppler or subsystem alarm.
        let mut sim = SessionSim::new(3).unwrap();
        sim.set_scenario(Scenario::PointingError);
        let seen = run(&mut sim, 200);
        assert_eq!(seen & !0x08, 0, "saw {seen:#010b}");
    }

    #[test]
    fn returning_to_nominal_clears_alarms() {
        let mut sim = SessionSim::new(1).unwrap();
        sim.set_scenario(Scenario::Thermal);
        assert_ne!(run(&mut sim, 5), 0);
        sim.set_scenario(Scenario::Nominal);
        assert_eq!(run(&mut sim, 50), 0);
    }

    #[test]
    fn horizon_mask_toggle_flags_below_horizon() {
        let mut sim = SessionSim::new(5).unwrap();
        assert_eq!(run(&mut sim, 100) & 0x02, 0, "mask off: no horizon flag");
        sim.set_horizon_mask(true).unwrap();
        assert!(sim.mask_on());
        assert_ne!(
            run(&mut sim, ORBIT_TICKS) & 0x02,
            0,
            "mask on: below horizon seen"
        );
    }

    #[test]
    fn sessions_are_independent() {
        let mut a = SessionSim::new(1).unwrap();
        let mut b = SessionSim::new(1).unwrap();
        a.set_scenario(Scenario::BodyRate);
        assert_ne!(run(&mut a, 3) & 0x40, 0);
        assert_eq!(run(&mut b, 3), 0);
    }

    #[test]
    fn speed_is_clamped_and_reset_restarts() {
        let mut sim = SessionSim::new(1).unwrap();
        sim.set_speed(1e9);
        assert_eq!(sim.speed(), SPEED_RANGE.1);
        sim.set_speed(f64::NAN);
        assert_eq!(sim.speed(), SPEED_RANGE.1);
        sim.set_speed(-5.0);
        assert_eq!(sim.speed(), SPEED_RANGE.0);
        run(&mut sim, 10);
        sim.reset();
        assert_eq!(sim.sim_time(), sandbox_start_utc());
        assert_eq!(sim.scenario(), Scenario::Nominal);
    }

    #[test]
    fn starts_shortly_before_an_overhead_pass() {
        let mut sim = SessionSim::new(9).unwrap();
        let (mut first_up, mut max_el) = (None, f64::MIN);
        for i in 0..400 {
            let out = sim.step(0.1).unwrap();
            let t: serde_json::Value = serde_json::from_str(&out.tick_json).unwrap();
            let el = t["expected"]["elevation_deg"].as_f64().unwrap();
            if el > 0.0 && first_up.is_none() {
                first_up = Some(i);
            }
            max_el = max_el.max(el);
        }
        let first_up = first_up.expect("spacecraft rises within 40 s of wall time at 60x");
        assert!(first_up < 300, "rises after {first_up} ticks");
        assert!(max_el > 80.0, "overhead pass, got max elevation {max_el}");
    }

    #[test]
    fn frame_json_matches_gateway_contract() {
        let mut sim = SessionSim::new(1).unwrap();
        let out = sim.step(0.1).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out.frame_json).unwrap();
        assert_eq!(v["chronus_schema"], "openmct.realtime.v1");
        assert_eq!(v["apid"], SANDBOX_APID);
        assert!(v["elevation_deg"].is_f64());
        let t: serde_json::Value = serde_json::from_str(&out.tick_json).unwrap();
        assert_eq!(t["chronus_schema"], "chronus.sandbox.v1");
        assert_eq!(t["scenario"], "nominal");
    }
}
