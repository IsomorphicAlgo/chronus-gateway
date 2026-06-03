//! NeXosim-backed **synthetic** spacecraft telemetry for Milestone 7 HIL.
//!
//! Drives [`chronus_gateway`] over UDP with CCSDS TM packets carrying abstract EPS / thermal /
//! ADCS scalars (no mission-specific data; see `AGENTS.md`).
//!
//! **Credit:** [NeXosim](https://github.com/asynchronics/nexosim) (asynchronics) — MIT OR Apache-2.0.

use std::net::SocketAddr;

use chronus_gateway::encode_synthetic_tm;
use nexosim::model::{BuildContext, Context, Model, ProtoModel, schedulable};
use nexosim::ports::Output;
use nexosim::simulation::{ExecutionError, Mailbox, SimInit};
use nexosim::time::MonotonicTime;
use serde::{Deserialize, Serialize};

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

impl TelemSample {
    /// Packs the sample into a CCSDS packet data field (big-endian scalars).
    pub fn to_payload_bytes(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(16);
        b.extend_from_slice(&self.seq.to_be_bytes());
        b.extend_from_slice(&self.eps_bus_voltage_v.to_be_bytes());
        b.extend_from_slice(&self.thermal_panel_c.to_be_bytes());
        b.extend_from_slice(&self.body_rate_deg_s.to_be_bytes());
        b
    }
}

/// Discrete-event “spacecraft” that emits [`TelemSample`] on `downlink` at 1 ms simulation steps.
#[derive(Serialize, Deserialize)]
pub struct SpacecraftDemo {
    pub downlink: Output<TelemSample>,
    pub total: u32,
    pub emitted: u32,
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
        let t = self.emitted as f32 * 0.1;
        let sample = TelemSample {
            seq: self.emitted,
            eps_bus_voltage_v: 28.0 + 0.05 * t.sin(),
            thermal_panel_c: 22.0 + 0.02 * t.cos(),
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
    async fn recv_sample(
        &mut self,
        sample: TelemSample,
        _cx: &Context<Self>,
        env: &mut BridgeEnv,
    ) {
        let payload = sample.to_payload_bytes();
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
/// `apid` is a synthetic APID (default in binary: `0x7B0`). Uses **simulation** time steps of 1 ms
/// between frames (not wall clock), so this returns quickly for tests.
pub fn run_nexosim_udp_hil(dest: SocketAddr, total_frames: u32, apid: u16) -> anyhow::Result<()> {
    let mut sc = SpacecraftDemo {
        downlink: Output::default(),
        total: total_frames,
        emitted: 0,
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

    sim.run().map_err(|e: ExecutionError| anyhow::anyhow!("NeXosim run: {e:?}"))?;
    Ok(())
}
