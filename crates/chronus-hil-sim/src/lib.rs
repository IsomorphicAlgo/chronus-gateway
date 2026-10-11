//! NeXosim-backed **synthetic** spacecraft telemetry for Milestone 7 HIL.
//!
//! Drives [`chronus_gateway`] over UDP with CCSDS TM packets whose **data field** uses the
//! versioned **`chronus.hil.tm.v1`** layout (`chronus_gateway::hil_tm`) for abstract EPS / thermal /
//! ADCS scalars (no mission-specific data; synthetic demo only). **CV-4:** EPS voltage and panel
//! temperature follow the same linear **Sun-illumination** factor as the gateway’s
//! [`chronus_gateway::nadir_sun_illumination_cos`] check so HIL passes are self-consistent with
//! Ephemerust SGP4 + the conical umbra/penumbra eclipse model (Ephemerust 0.7.0, Methodology
//! **D-038**; public ISS TLE only).
//!
//! **Credit:** [NeXosim](https://github.com/asynchronics/nexosim) (asynchronics) — MIT OR Apache-2.0.
//!
//! ## Scripted anomaly (Showcase S3)
//!
//! Optional [`HilScriptedAnomaly`] injects **synthetic** out-of-band EPS, thermal, or body-rate values
//! for a bounded window of frame indices so the gateway’s **CV-4 / CV-5** checks set `physics_flags`
//! bits **4–6** — useful for repeatable alarm demos without editing replay fixtures.

// Unsafe-free by construction (gateway Tranche R finding F-2).
#![forbid(unsafe_code)]

pub mod rf;

use std::net::SocketAddr;
use std::sync::OnceLock;

use chrono::{Duration, TimeZone, Utc};
use chronus_gateway::config::DEFAULT_ISS_TLE;
use chronus_gateway::hil_tm::encode_hil_tm_v1_payload;
use chronus_gateway::nadir_sun_illumination_cos;
use chronus_gateway::time::{TimeFormat, encode_time};
use chronus_gateway::{encode_synthetic_tm, encode_synthetic_tm_with_time};
use ephemerust::satellite::Tle;
use nexosim::model::{BuildContext, Context, Model, ProtoModel, schedulable};
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
pub fn hil_targets_from_illum(illum: f64) -> (f32, f32) {
    const V_SUN: f64 = 28.0;
    const V_ECL: f64 = 24.0;
    const T_HOT: f64 = 32.0;
    const T_COLD: f64 = 12.0;
    let clamped = illum.clamp(0.0, 1.0);
    let v = V_ECL + (V_SUN - V_ECL) * clamped;
    let t = T_COLD + (T_HOT - T_COLD) * clamped;
    (v as f32, t as f32)
}

/// UTC instant that HIL frame `0` maps to (matches the public ISS TLE epoch used by the gateway default).
#[must_use]
pub fn default_hil_epoch_utc() -> chrono::DateTime<chrono::Utc> {
    Utc.with_ymd_and_hms(2020, 7, 12, 21, 0, 0)
        .single()
        .expect("static epoch is a valid UTC instant")
}

/// Nominal (fault-free) subsystem snapshot for frame `seq` at simulated time `utc`.
///
/// EPS voltage and panel temperature follow the same Sun-illumination map the gateway uses for
/// **CV-4**, so a nominal sample validated at the **same** `utc` raises no subsystem alarms.
/// Shared by the NeXosim driver and the per-visitor sandbox so both stay self-consistent.
#[must_use]
pub fn nominal_sample(seq: u32, utc: chrono::DateTime<chrono::Utc>) -> TelemSample {
    let illum = nadir_sun_illumination_cos(iss_tle_cached(), utc).unwrap_or(0.0);
    let (eps_bus_voltage_v, thermal_panel_c) = hil_targets_from_illum(illum);
    TelemSample {
        seq,
        eps_bus_voltage_v,
        thermal_panel_c,
        body_rate_deg_s: 0.001 * seq as f32,
    }
}

/// One synthetic “subsystem snapshot” per telemetry frame (generic floats, not flight hardware).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TelemSample {
    /// Monotonic frame index in this run.
    pub seq: u32,
    /// Abstract EPS bus voltage \[V\].
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
        let mut sample = nominal_sample(self.emitted, utc);
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

/// Everything one HIL run is configured with.
///
/// [`HilRunOptions::new`] gives the legacy behaviour (no time field, no RF sidecar, unpaced), so
/// existing callers are unaffected; the `with_*` methods opt in to the Tranche 2 features.
#[derive(Clone, Debug)]
pub struct HilRunOptions {
    /// Gateway UDP ingest address.
    pub dest: SocketAddr,
    /// Number of telemetry frames.
    pub frames: u32,
    /// CCSDS APID (inside the gateway's HIL band).
    pub apid: u16,
    /// Subsystem fault window (bits 4–6).
    pub script: Option<HilScriptedAnomaly>,
    /// Mission time of frame 0 (also what the onboard time code carries).
    pub epoch_utc: chrono::DateTime<chrono::Utc>,
    /// Time code to put in each packet's secondary header (`None` = legacy packets without one).
    pub time_format: TimeFormat,
    /// TAI−UTC used when encoding CUC.
    pub tai_minus_utc_s: i32,
    /// Gateway `rf.bind_addr`: send a consistent `chronus.rf.v1` sample before every frame.
    pub rf_dest: Option<SocketAddr>,
    /// Receiver fault window (bits 0, 2, 3).
    pub rf_script: Option<rf::RfScriptedAnomaly>,
    /// Wall-clock pause after each frame (ms). RF matching is by arrival time, so runs that send
    /// RF samples must be paced (see [`HilRunOptions::with_rf`]).
    pub frame_interval_ms: u64,
    /// Pause between an RF sample and its frame (ms).
    pub rf_lead_ms: u64,
}

impl HilRunOptions {
    /// Legacy behaviour: nominal HIL packets, no time field, no RF, unpaced.
    #[must_use]
    pub fn new(dest: SocketAddr, frames: u32, apid: u16) -> Self {
        Self {
            dest,
            frames,
            apid,
            script: None,
            epoch_utc: default_hil_epoch_utc(),
            time_format: TimeFormat::None,
            tai_minus_utc_s: 37,
            rf_dest: None,
            rf_script: None,
            frame_interval_ms: 0,
            rf_lead_ms: 0,
        }
    }

    /// Sets the subsystem fault window.
    #[must_use]
    pub fn with_script(mut self, script: Option<HilScriptedAnomaly>) -> Self {
        self.script = script;
        self
    }

    /// Stamps every packet with an onboard time code and sets mission time of frame 0.
    #[must_use]
    pub fn with_time(
        mut self,
        format: TimeFormat,
        epoch_utc: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        self.time_format = format;
        self.epoch_utc = epoch_utc;
        self
    }

    /// Enables the RF sidecar, paced so each sample is matched to its own frame
    /// (20 ms between frames, 5 ms lead).
    #[must_use]
    pub fn with_rf(
        mut self,
        rf_dest: SocketAddr,
        rf_script: Option<rf::RfScriptedAnomaly>,
    ) -> Self {
        self.rf_dest = Some(rf_dest);
        self.rf_script = rf_script;
        self.frame_interval_ms = 20;
        self.rf_lead_ms = 5;
        self
    }
}

/// Environment for [`UdpDownlinkBridge`]: bound UDP socket + run options.
pub struct BridgeEnv {
    sock: std::net::UdpSocket,
    opts: HilRunOptions,
    rf_model: Option<rf::RfModel>,
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
        let o = &env.opts;
        let utc = o.epoch_utc + Duration::milliseconds(i64::from(sample.seq));

        // Receiver sidecar first, so the gateway holds the sample when the frame arrives.
        if let (Some(rf_dest), Some(model)) = (o.rf_dest, env.rf_model.as_ref())
            && let Some(mut rf_sample) = model.nominal(utc)
        {
            if let Some(script) = o.rf_script.as_ref() {
                rf::apply_rf_anomaly(&mut rf_sample, sample.seq, script);
            }
            let _ = env.sock.send_to(&rf_sample.to_datagram(), rf_dest);
            if o.rf_lead_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(o.rf_lead_ms));
            }
        }

        let pkt = match encode_time(o.time_format, utc, o.tai_minus_utc_s) {
            Some(field) => encode_synthetic_tm_with_time(o.apid, seq, &field, &payload),
            None => encode_synthetic_tm(o.apid, seq, &payload),
        };
        let _ = env.sock.send_to(&pkt, o.dest);
        if o.frame_interval_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(o.frame_interval_ms));
        }
    }
}

/// Prototype for [`UdpDownlinkBridge`] (owns UDP configuration before `build`).
pub struct ProtoUdpBridge {
    pub opts: HilRunOptions,
}

impl ProtoModel for ProtoUdpBridge {
    type Model = UdpDownlinkBridge;

    fn build(self, _cx: &mut BuildContext<Self>) -> (Self::Model, BridgeEnv) {
        let sock = std::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, 0))
            .expect("HIL: bind ephemeral UDP for downlink");
        let rf_model = self.opts.rf_dest.map(|_| {
            rf::RfModel::from_station(&chronus_gateway::config::StationConfig::default())
                .expect("HIL: default station builds an RF model")
        });
        let model = UdpDownlinkBridge;
        let env = BridgeEnv {
            sock,
            opts: self.opts,
            rf_model,
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
    run_nexosim_hil(HilRunOptions::new(dest, total_frames, apid).with_script(scripted_anomaly))
}

/// Full-featured run: onboard time codes, the `chronus.rf.v1` RF sidecar and both fault scripts.
/// See [`HilRunOptions`].
pub fn run_nexosim_hil(opts: HilRunOptions) -> anyhow::Result<()> {
    let mut sc = SpacecraftDemo {
        downlink: Output::default(),
        total: opts.frames,
        emitted: 0,
        hil_epoch_utc: opts.epoch_utc,
        sim_step_ms: 1,
        scripted_anomaly: opts.script,
    };
    let proto = ProtoUdpBridge { opts };
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
