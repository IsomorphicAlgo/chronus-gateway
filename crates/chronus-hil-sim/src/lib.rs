//! NeXosim-backed **synthetic** spacecraft telemetry for Milestone 7 HIL.
//!
//! Drives [`chronus_gateway`] over UDP with CCSDS TM packets whose **data field** uses the
//! versioned **`chronus.hil.tm.v1`** layout (`chronus_gateway::hil_tm`) for abstract EPS / thermal /
//! ADCS scalars (no mission-specific data; synthetic demo only). **CV-4:** EPS voltage and panel
//! temperature follow the same linear **Sun-illumination** proxy as the gateway’s
//! [`chronus_gateway::nadir_sun_illumination_cos`] check so HIL passes are self-consistent with
//! Ephemerust SGP4 + low-precision Sun geometry (public ISS TLE only).
//!
//! **Credit:** [NeXosim](https://github.com/asynchronics/nexosim) (asynchronics) — MIT OR Apache-2.0.
//!
//! ## Scripted anomaly (Showcase S3)
//!
//! Optional [`HilScriptedAnomaly`] injects **synthetic** out-of-band EPS, thermal, or body-rate values
//! for a bounded window of frame indices so the gateway’s **CV-4 / CV-5** checks set `physics_flags`
//! bits **4–6** — useful for repeatable alarm demos without editing replay fixtures.

use std::net::SocketAddr;
use std::sync::OnceLock;

use chrono::{Duration, TimeZone, Utc};
use chronus_gateway::encode_synthetic_tm;
use chronus_gateway::config::DEFAULT_ISS_TLE;
use chronus_gateway::hil_tm::encode_hil_tm_v1_payload;
use chronus_gateway::nadir_sun_illumination_cos;
use ephemerust::satellite::Tle;
use nexosim::model::{schedulable, BuildContext, Context, Model, ProtoModel};
use nexosim::ports::Output;
use nexosim::simulation::{ExecutionError, Mailbox, SimInit};
use nexosim::time::MonotonicTime;
use serde::{Deserialize, Serialize};

/// Which **synthetic** HIL scalar is forced out of band for a frame window (**CV-4** / **CV-5**).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HilScriptedAnomalyKind {
    /// EPS bus voltage incompatible with the Sun-illumination linear model (`physics_flags` bit **4**).
    EpsVoltage,
    /// Panel temperature incompatible with the illumination model (bit **5**).
    Thermal,
    /// Body rate magnitude above default **T-BODYRATE** (bit **6**).
    BodyRate,
}

/// Inject a fixed fault for frames **`start_frame` .. `start_frame + duration_frames`** (half-open end).
///
/// `duration_frames == 0` disables injection. Values are **lab-only** exaggerations vs
/// [`StationConfig::default`](chronus_gateway::config::StationConfig::default) CV-4/CV-5 tolerances.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HilScriptedAnomaly {
    pub kind: HilScriptedAnomalyKind,
    /// First `TelemSample::seq` (same as simulator `emitted` at send time) to corrupt; inclusive.
    pub start_frame: u32,
    /// Number of consecutive frames; must be **> 0** for any injection.
    pub duration_frames: u32,
}

/// In-place mutation when `frame_index` lies in the scripted window. Returns **true** if `sample` was modified.
pub fn apply_scripted_anomaly(
    sample: &mut TelemSample,
    frame_index: u32,
    script: &HilScriptedAnomaly,
) -> bool {
    if script.duration_frames == 0 {
        return false;
    }
    let end = script.start_frame.saturating_add(script.duration_frames);
    if frame_index < script.start_frame || frame_index >= end {
        return false;
    }
    match script.kind {
        HilScriptedAnomalyKind::EpsVoltage => {
            // Default CV-4 band is ~24–28 V from illumination; 10 V is safely out of ±10 % of span.
            sample.eps_bus_voltage_v = 10.0;
        }
        HilScriptedAnomalyKind::Thermal => {
            // Default thermal map tops near 32 °C; 80 °C exceeds T-THERMAL (10 K band around model).
            sample.thermal_panel_c = 80.0;
        }
        HilScriptedAnomalyKind::BodyRate => {
            // Default ceiling is 5 deg/s; 99.0 triggers CV-5 bit 6.
            sample.body_rate_deg_s = 99.0;
        }
    }
    true
}

fn iss_tle_cached() -> &'static Tle {
    static CELL: OnceLock<Tle> = OnceLock::new();
    CELL.get_or_init(|| Tle::parse(DEFAULT_ISS_TLE).expect("default ISS TLE parses"))
}

/// Linear maps matching default [`chronus_gateway::config::StationConfig`] CV-4 toy endpoints.
fn hil_targets_from_illum(illum: f64) -> (f32, f32) {
    const V_SUN: f64 = 28.0;
    const V_ECL: f64 = 24.0;
    const T_HOT: f64 = 32.0;
    const T_COLD: f64 = 12.0;
    let clamped = illum.clamp(0.0, 1.0);
    let v = V_ECL + (V_SUN - V_ECL) * clamped;
    let t = T_COLD + (T_HOT - T_COLD) * clamped;
    (v as f32, t as f32)
}

/// One synthetic “subsystem snapshot” per telemetry frame (generic floats, not flight hardware).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TelemSample {
    /// Monotonic frame index in this run.
    pub seq: u32,
    /// Abstract EPS bus voltage [V].
    pub eps_bus_voltage_v: f32,
    /// Abstract panel temperature [°C].
    pub thermal_panel_c: f32,
    /// Abstract body rate about a nominal axis [deg/s].
    pub body_rate_deg_s: f32,
}

/// Discrete-event “spacecraft” that emits [`TelemSample`] on `downlink` at 1 ms simulation steps.
#[derive(Serialize, Deserialize)]
pub struct SpacecraftDemo {
    pub downlink: Output<TelemSample>,
    pub total: u32,
    pub emitted: u32,
    /// UTC time mapped to simulation frame `seq == 0` (CV-4 geometry alignment).
    pub hil_epoch_utc: chrono::DateTime<chrono::Utc>,
    /// Simulation time advance per frame (same units as NeXosim scheduling step).
    pub sim_step_ms: u64,
    /// Optional Showcase S3 fault injection window (`None` = nominal self-consistent HIL only).
    pub scripted_anomaly: Option<HilScriptedAnomaly>,
}

#[Model]
impl SpacecraftDemo {
    #[nexosim(init)]
    async fn init(&mut self, cx: &Context<Self>) {
        self.emitted = 0;
        // NeXosim rejects scheduling at exactly the current instant (zero delay).
        cx.schedule_event(
            std::time::Duration::from_nanos(1),
            schedulable!(Self::tick),
            (),
        )
        .expect("schedule first HIL tick");
    }

    #[nexosim(schedulable)]
    async fn tick(&mut self, (): (), cx: &Context<Self>) {
        if self.emitted >= self.total {
            return;
        }
        let utc = self.hil_epoch_utc
            + Duration::milliseconds(self.emitted as i64 * self.sim_step_ms as i64);
        let illum = nadir_sun_illumination_cos(iss_tle_cached(), utc).unwrap_or(0.0) as f64;
        let (eps_bus_voltage_v, thermal_panel_c) = hil_targets_from_illum(illum);
        let mut sample = TelemSample {
            seq: self.emitted,
            eps_bus_voltage_v,
            thermal_panel_c,
            body_rate_deg_s: 0.001 * self.emitted as f32,
        };
        if let Some(ref script) = self.scripted_anomaly {
            apply_scripted_anomaly(&mut sample, self.emitted, script);
        }
        self.downlink.send(sample).await;
        self.emitted += 1;
        if self.emitted < self.total {
            cx.schedule_event(
                std::time::Duration::from_millis(1),
                schedulable!(Self::tick),
                (),
            )
            .expect("schedule HIL tick");
        }
    }
}

/// Environment for [`UdpDownlinkBridge`]: bound UDP socket + gateway destination.
pub struct BridgeEnv {
    sock: std::net::UdpSocket,
    dest: SocketAddr,
    apid: u16,
}

/// Receives [`TelemSample`] from the simulation and sends CCSDS TM datagrams to the gateway.
#[derive(Serialize, Deserialize)]
pub struct UdpDownlinkBridge;

#[Model(type Env = BridgeEnv)]
impl UdpDownlinkBridge {
    #[nexosim(schedulable)]
    async fn recv_sample(&mut self, sample: TelemSample, _cx: &Context<Self>, env: &mut BridgeEnv) {
        let payload = encode_hil_tm_v1_payload(
            sample.seq,
            sample.eps_bus_voltage_v,
            sample.thermal_panel_c,
            sample.body_rate_deg_s,
        );
        let seq = (sample.seq & 0x3FFF) as u16;
        let pkt = encode_synthetic_tm(env.apid, seq, &payload);
        let _ = env.sock.send_to(&pkt, env.dest);
    }
}

/// Prototype for [`UdpDownlinkBridge`] (owns UDP configuration before `build`).
pub struct ProtoUdpBridge {
    pub dest: SocketAddr,
    pub apid: u16,
}

impl ProtoModel for ProtoUdpBridge {
    type Model = UdpDownlinkBridge;

    fn build(self, _cx: &mut BuildContext<Self>) -> (Self::Model, BridgeEnv) {
        let sock = std::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, 0))
            .expect("HIL: bind ephemeral UDP for downlink");
        let model = UdpDownlinkBridge;
        let env = BridgeEnv {
            sock,
            dest: self.dest,
            apid: self.apid,
        };
        (model, env)
    }
}

/// Runs the NeXosim bench until all scheduled telemetry is emitted and UDP sends complete.
///
/// `apid` must fall within the gateway’s configured **HIL TM v1** APID band (default **0x7B0…0x7BF**).
/// Scheduling uses short simulated delays between frames (not wall clock), so this returns quickly for tests.
pub fn run_nexosim_udp_hil(dest: SocketAddr, total_frames: u32, apid: u16) -> anyhow::Result<()> {
    run_nexosim_udp_hil_with_script(dest, total_frames, apid, None)
}

/// Like [`run_nexosim_udp_hil`] but with optional [`HilScriptedAnomaly`] for deterministic CV alarms.
pub fn run_nexosim_udp_hil_with_script(
    dest: SocketAddr,
    total_frames: u32,
    apid: u16,
    scripted_anomaly: Option<HilScriptedAnomaly>,
) -> anyhow::Result<()> {
    let mut sc = SpacecraftDemo {
        downlink: Output::default(),
        total: total_frames,
        emitted: 0,
        hil_epoch_utc: Utc.with_ymd_and_hms(2020, 7, 12, 21, 0, 0).unwrap(),
        sim_step_ms: 1,
        scripted_anomaly,
    };
    let proto = ProtoUdpBridge { dest, apid };
    let sc_mbox = Mailbox::new();
    let br_mbox = Mailbox::new();
    sc.downlink
        .connect(UdpDownlinkBridge::recv_sample, &br_mbox);

    let t0 = MonotonicTime::EPOCH;
    let mut sim = SimInit::new()
        .add_model(proto, br_mbox, "hil_udp_bridge")
        .add_model(sc, sc_mbox, "hil_spacecraft")
        .init(t0)
        .map_err(|e| anyhow::anyhow!("NeXosim bench init: {e:?}"))?;

    sim.run()
        .map_err(|e: ExecutionError| anyhow::anyhow!("NeXosim run: {e:?}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_scripted_anomaly_respects_window() {
        let script = HilScriptedAnomaly {
            kind: HilScriptedAnomalyKind::BodyRate,
            start_frame: 3,
            duration_frames: 2,
        };
        let mut s = TelemSample {
            seq: 3,
            eps_bus_voltage_v: 26.0,
            thermal_panel_c: 20.0,
            body_rate_deg_s: 0.1,
        };
        assert!(apply_scripted_anomaly(&mut s, 3, &script));
        assert_eq!(s.body_rate_deg_s, 99.0);

        let mut s2 = TelemSample {
            seq: 4,
            eps_bus_voltage_v: 26.0,
            thermal_panel_c: 20.0,
            body_rate_deg_s: 0.2,
        };
        assert!(apply_scripted_anomaly(&mut s2, 4, &script));
        assert_eq!(s2.body_rate_deg_s, 99.0);

        let mut s3 = TelemSample {
            seq: 2,
            eps_bus_voltage_v: 26.0,
            thermal_panel_c: 20.0,
            body_rate_deg_s: 0.05,
        };
        assert!(!apply_scripted_anomaly(&mut s3, 2, &script));
        assert_eq!(s3.body_rate_deg_s, 0.05);

        let mut s4 = TelemSample {
            seq: 5,
            eps_bus_voltage_v: 26.0,
            thermal_panel_c: 20.0,
            body_rate_deg_s: 0.3,
        };
        assert!(!apply_scripted_anomaly(&mut s4, 5, &script));
        assert_eq!(s4.body_rate_deg_s, 0.3);
    }

    #[test]
    fn zero_duration_never_injects() {
        let script = HilScriptedAnomaly {
            kind: HilScriptedAnomalyKind::EpsVoltage,
            start_frame: 0,
            duration_frames: 0,
        };
        let mut s = TelemSample {
            seq: 0,
            eps_bus_voltage_v: 26.0,
            thermal_panel_c: 20.0,
            body_rate_deg_s: 0.0,
        };
        assert!(!apply_scripted_anomaly(&mut s, 0, &script));
        assert_eq!(s.eps_bus_voltage_v, 26.0);
    }
}
