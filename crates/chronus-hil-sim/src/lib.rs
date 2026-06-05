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
        let sample = TelemSample {
            seq: self.emitted,
            eps_bus_voltage_v,
            thermal_panel_c,
            body_rate_deg_s: 0.001 * self.emitted as f32,
        };
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
/// between frames (not wall clock), so this returns quickly for tests.
pub fn run_nexosim_udp_hil(dest: SocketAddr, total_frames: u32, apid: u16) -> anyhow::Result<()> {
    let mut sc = SpacecraftDemo {
        downlink: Output::default(),
        total: total_frames,
        emitted: 0,
        hil_epoch_utc: Utc.with_ymd_and_hms(2020, 7, 12, 21, 0, 0).unwrap(),
        sim_step_ms: 1,
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
