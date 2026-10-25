//! Replay synthetic CCSDS TM **UDP datagrams** from a text fixture (Showcase S3), optionally with a
//! `chronus.rf.v1` receiver-metadata sidecar (Tranche 2).
//!
//! Each non-empty, non-`#` line is either:
//! - **Hex:** whitespace-separated or contiguous hex (one full Space Packet per line).
//! - **JSONL:** a JSON object. Either **`udp_hex`** (contiguous hex, one datagram) **or** a packet
//!   description — **`apid`**, optional **`seq`**, **`data_hex`**, and **`onboard_time`** (RFC 3339,
//!   or `"now"` to stamp the send time) — which the tool encodes with a CDS/CUC time field
//!   (`--time-format`) and the secondary-header flag set.
//!
//! A JSONL line may also carry an **`rf`** object (`carrier_hz`, `rx_power_dbm`, `az_deg`,
//! `el_deg`, optional `t`). With `--rf-dest HOST:PORT` it is sent as one `chronus.rf.v1` datagram
//! just **before** its packet, which is how the gateway's Doppler, link-budget and pointing checks
//! are driven from a recording.
//!
//! With **`--tm-frames`** every packet is wrapped in CCSDS TM Transfer Frames before sending
//! (Tranche 3): frames of `--frame-length` octets from spacecraft `--scid` on virtual channel
//! `--vcid`, with a CRC (unless `--no-fecf`), optionally preceded by the sync marker (`--asm`) and
//! randomized (`--randomize`). Each line's packet is flushed into its own frame(s), padded with an
//! idle packet, so receiver samples still line up in time. Fixtures that already hold frames or
//! CADUs (for example `tm_frames_lab.hex`) are replayed as-is, without `--tm-frames`.
//!
//! **Credit:** [`clap`](https://crates.io/crates/clap), [`tokio`](https://crates.io/crates/tokio), [`anyhow`](https://crates.io/crates/anyhow), [`serde_json`](https://crates.io/crates/serde_json) — see repository `README.md` § Acknowledgements.

#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use chronus_gateway::ccsds::encode_synthetic_tm_with_time;
use chronus_gateway::frames::{TmFrameBuilder, ccsds_randomize, to_cadu};
use chronus_gateway::rf::RfSampleV1;
use chronus_gateway::time::{TimeFormat, encode_time};
use clap::{Parser, ValueEnum};
use tokio::fs::read_to_string;
use tokio::net::UdpSocket;
use tokio::time::sleep;

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, ValueEnum)]
enum TimeFormatCli {
    /// CCSDS Day Segmented time (UTC).
    #[default]
    Cds,
    /// CCSDS Unsegmented time (TAI).
    Cuc,
}

#[derive(Parser, Debug)]
#[command(
    name = "chronus-replay",
    version,
    about = "Replay UDP CCSDS TM datagrams (and optional chronus.rf.v1 receiver metadata) from a hex/JSONL fixture"
)]
struct Cli {
    /// Fixture file (hex lines or JSONL; see the crate docs for the accepted keys).
    #[arg(short = 'f', long = "file")]
    file: PathBuf,

    /// Destination `HOST:PORT` (gateway UDP ingest).
    #[arg(default_value = "127.0.0.1:7301")]
    dest: SocketAddr,

    /// Pause between datagrams (milliseconds).
    #[arg(long, default_value_t = 0)]
    delay_ms: u64,

    /// Send the whole sequence this many times (deterministic “same bytes again” demos).
    #[arg(long, default_value_t = 1)]
    repeat: u32,

    /// Gateway `rf.bind_addr` (`HOST:PORT`): send each line's `rf` object there as `chronus.rf.v1`.
    #[arg(long)]
    rf_dest: Option<SocketAddr>,

    /// Pause between an RF sample and its packet (milliseconds), so the sample is stored first.
    #[arg(long, default_value_t = 5)]
    rf_lead_ms: u64,

    /// Time code used for lines that give `onboard_time` (must match the gateway's `time.format`).
    #[arg(long, value_enum, default_value_t = TimeFormatCli::Cds)]
    time_format: TimeFormatCli,

    /// TAI−UTC seconds for CUC encoding (must match the gateway's `time.cuc_tai_minus_utc_s`).
    #[arg(long, default_value_t = 37)]
    tai_minus_utc_s: i32,

    /// Wrap packets in CCSDS TM Transfer Frames (gateway: `[framing] mode = "tm_frames"`).
    #[arg(long)]
    tm_frames: bool,

    /// Transfer frame length in octets, header through CRC (with `--tm-frames`).
    #[arg(long, default_value_t = 223)]
    frame_length: usize,

    /// Spacecraft identifier written in the frame headers (0..=1023, with `--tm-frames`).
    #[arg(long, default_value_t = 0x2A)]
    scid: u16,

    /// Virtual channel the packets travel on (0..=7, with `--tm-frames`).
    #[arg(long, default_value_t = 0)]
    vcid: u8,

    /// Prefix each frame with the sync marker 1ACFFC1D (gateway: `framing.asm = true`).
    #[arg(long)]
    asm: bool,

    /// XOR frames with the CCSDS pseudo-random sequence (gateway: `framing.derandomize = true`).
    #[arg(long)]
    randomize: bool,

    /// Leave the 2-octet frame CRC off (gateway: `framing.fecf = false`).
    #[arg(long)]
    no_fecf: bool,
}

/// Frame-wrapping options taken from the command line.
#[derive(Debug, Clone, Copy)]
struct FrameOpts {
    vcid: u8,
    asm: bool,
    randomize: bool,
}

impl Cli {
    /// A frame builder when `--tm-frames` is set, after checking the frame options.
    fn framer(&self) -> Result<Option<(TmFrameBuilder, FrameOpts)>> {
        if !self.tm_frames {
            return Ok(None);
        }
        if !(chronus_gateway::frames::TM_FRAME_MIN_LEN + 2
            ..=chronus_gateway::frames::TM_FRAME_MAX_LEN)
            .contains(&self.frame_length)
        {
            bail!(
                "--frame-length must be 10..=2048 octets, got {}",
                self.frame_length
            );
        }
        if self.scid > 0x3FF {
            bail!("--scid must be 0..=1023, got {}", self.scid);
        }
        if self.vcid > 7 {
            bail!("--vcid must be 0..=7, got {}", self.vcid);
        }
        let b = TmFrameBuilder::new(self.scid, self.frame_length).with_fecf(!self.no_fecf);
        Ok(Some((
            b,
            FrameOpts {
                vcid: self.vcid,
                asm: self.asm,
                randomize: self.randomize,
            },
        )))
    }
}

/// Wraps one packet into its own frame(s) and returns the datagrams to send.
fn frame_packet(builder: &mut TmFrameBuilder, opts: FrameOpts, packet: &[u8]) -> Vec<Vec<u8>> {
    let mut frames = builder.pack(opts.vcid, &[packet]);
    frames.extend(builder.flush(opts.vcid));
    frames
        .into_iter()
        .map(|mut f| {
            if opts.asm {
                to_cadu(&f, opts.randomize)
            } else {
                if opts.randomize {
                    ccsds_randomize(&mut f);
                }
                f
            }
        })
        .collect()
}

/// When a described packet's time field is stamped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnboardTime {
    /// The moment the datagram is sent.
    Now,
    /// A fixed instant (recorded or simulated stream).
    At(DateTime<Utc>),
}

/// How a line's packet is obtained.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PacketSpec {
    /// Complete datagram bytes, sent as-is.
    Raw(Vec<u8>),
    /// A packet encoded at send time with a leading time field.
    Timed {
        apid: u16,
        seq: u16,
        time: OnboardTime,
        data: Vec<u8>,
    },
}

/// One fixture line: a packet and, optionally, the receiver measurements that accompany it.
#[derive(Debug, Clone, PartialEq)]
struct Item {
    packet: PacketSpec,
    rf: Option<RfSampleV1>,
}

fn parse_hex_bytes(s: &str) -> Result<Vec<u8>> {
    let compact: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.is_empty() {
        bail!("empty hex");
    }
    if !compact.len().is_multiple_of(2) {
        bail!("hex length must be even, got {}", compact.len());
    }
    let mut out = Vec::with_capacity(compact.len() / 2);
    for chunk in compact.as_bytes().chunks(2) {
        let pair = std::str::from_utf8(chunk).context("hex must be ASCII")?;
        let b =
            u8::from_str_radix(pair, 16).with_context(|| format!("invalid hex pair `{pair}`"))?;
        out.push(b);
    }
    Ok(out)
}

fn json_f64(v: &serde_json::Value, key: &str) -> Result<Option<f64>> {
    match v.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(x) => x
            .as_f64()
            .filter(|f| f.is_finite())
            .map(Some)
            .with_context(|| format!("`rf.{key}` must be a finite number")),
    }
}

fn parse_rf(v: &serde_json::Value) -> Result<RfSampleV1> {
    if !v.is_object() {
        bail!("`rf` must be a JSON object");
    }
    let mut s = RfSampleV1::new();
    if let Some(x) = json_f64(v, "carrier_hz")? {
        s = s.with_carrier_hz(x);
    }
    if let Some(x) = json_f64(v, "rx_power_dbm")? {
        s = s.with_rx_power_dbm(x);
    }
    match (json_f64(v, "az_deg")?, json_f64(v, "el_deg")?) {
        (Some(a), Some(e)) => s = s.with_pointing_deg(a, e),
        (None, None) => {}
        _ => bail!("`rf` needs both az_deg and el_deg for a pointing measurement"),
    }
    if let Some(t) = v.get("t").and_then(|x| x.as_str()) {
        let t: DateTime<Utc> = t.parse().context("`rf.t` must be RFC 3339")?;
        s = s.with_time(t);
    }
    Ok(s)
}

/// One logical line → one [`Item`], or `None` to skip (blank / comment).
fn parse_line(line: &str) -> Result<Option<Item>> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return Ok(None);
    }
    if !line.starts_with('{') {
        return Ok(Some(Item {
            packet: PacketSpec::Raw(parse_hex_bytes(line)?),
            rf: None,
        }));
    }
    let v: serde_json::Value = serde_json::from_str(line).context("invalid JSON line")?;
    let rf = v.get("rf").map(parse_rf).transpose()?;
    let packet = if let Some(hex) = v.get("udp_hex").and_then(|x| x.as_str()) {
        PacketSpec::Raw(parse_hex_bytes(hex)?)
    } else if let Some(apid) = v.get("apid").and_then(|x| x.as_u64()) {
        let apid = u16::try_from(apid)
            .ok()
            .filter(|a| *a <= 0x7FF)
            .context("`apid` must be 0..=2047")?;
        let seq = v.get("seq").and_then(|x| x.as_u64()).unwrap_or(0);
        let seq = u16::try_from(seq)
            .ok()
            .filter(|s| *s <= 0x3FFF)
            .context("`seq` must be 0..=16383")?;
        let data = parse_hex_bytes(
            v.get("data_hex")
                .and_then(|x| x.as_str())
                .context("packet description requires string field `data_hex`")?,
        )?;
        let time = match v.get("onboard_time").and_then(|x| x.as_str()) {
            None => bail!("packet description requires `onboard_time` (RFC 3339 or \"now\")"),
            Some("now") => OnboardTime::Now,
            Some(t) => OnboardTime::At(t.parse().context("`onboard_time` must be RFC 3339")?),
        };
        PacketSpec::Timed {
            apid,
            seq,
            time,
            data,
        }
    } else {
        bail!(
            "JSONL line requires string field `udp_hex` or a packet description (`apid`, `data_hex`, `onboard_time`)"
        );
    };
    Ok(Some(Item { packet, rf }))
}

impl PacketSpec {
    /// The datagram bytes to send now.
    fn materialize(&self, format: TimeFormat, tai_minus_utc_s: i32) -> Result<Vec<u8>> {
        match self {
            PacketSpec::Raw(b) => Ok(b.clone()),
            PacketSpec::Timed {
                apid,
                seq,
                time,
                data,
            } => {
                let t = match time {
                    OnboardTime::Now => Utc::now(),
                    OnboardTime::At(t) => *t,
                };
                let field = encode_time(format, t, tai_minus_utc_s)
                    .with_context(|| format!("onboard_time {t} is not representable"))?;
                Ok(encode_synthetic_tm_with_time(*apid, *seq, &field, data))
            }
        }
    }
}

fn load_items(text: &str) -> Result<Vec<Item>> {
    let mut out = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let n = idx + 1;
        match parse_line(line) {
            Ok(None) => {}
            Ok(Some(item)) => {
                if let PacketSpec::Raw(pkt) = &item.packet
                    && pkt.len() < 7
                {
                    bail!(
                        "line {n}: datagram too short for a CCSDS primary header ({})",
                        pkt.len()
                    );
                }
                out.push(item);
            }
            Err(e) => bail!("line {n}: {e:#}"),
        }
    }
    if out.is_empty() {
        bail!("no datagrams in fixture");
    }
    Ok(out)
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let text = read_to_string(&cli.file)
        .await
        .with_context(|| format!("read {}", cli.file.display()))?;
    let items = load_items(&text)?;

    let rf_lines = items.iter().filter(|i| i.rf.is_some()).count();
    if rf_lines > 0 && cli.rf_dest.is_none() {
        eprintln!(
            "chronus-replay: note: {rf_lines} line(s) carry `rf` measurements but --rf-dest is not set; \
             the gateway's Doppler/link-budget/pointing checks will not run"
        );
    }
    let format = match cli.time_format {
        TimeFormatCli::Cds => TimeFormat::Cds,
        TimeFormatCli::Cuc => TimeFormat::Cuc,
    };

    let mut framer = cli.framer()?;
    let mut sent = 0usize;

    let sock = UdpSocket::bind("0.0.0.0:0")
        .await
        .context("bind ephemeral UDP")?;
    let delay = Duration::from_millis(cli.delay_ms);
    let lead = Duration::from_millis(cli.rf_lead_ms);
    let mut rf_sent = 0usize;

    for r in 0..cli.repeat {
        if cli.repeat > 1 {
            eprintln!("chronus-replay: repeat {}/{}", r + 1, cli.repeat);
        }
        for (i, item) in items.iter().enumerate() {
            if let (Some(rf), Some(rf_dest)) = (item.rf.as_ref(), cli.rf_dest) {
                sock.send_to(&rf.to_datagram(), rf_dest)
                    .await
                    .with_context(|| format!("send rf sample {}", i + 1))?;
                rf_sent += 1;
                if cli.rf_lead_ms > 0 {
                    sleep(lead).await;
                }
            }
            let pkt = item.packet.materialize(format, cli.tai_minus_utc_s)?;
            let datagrams = match framer.as_mut() {
                Some((builder, opts)) => frame_packet(builder, *opts, &pkt),
                None => vec![pkt],
            };
            for dg in &datagrams {
                sock.send_to(dg, cli.dest)
                    .await
                    .with_context(|| format!("send datagram {}", i + 1))?;
                sent += 1;
            }
            if cli.delay_ms > 0 {
                sleep(delay).await;
            }
        }
    }

    eprintln!(
        "chronus-replay: sent {} line(s) × {} repeat(s) as {sent} datagram(s){} → {}{}",
        items.len(),
        cli.repeat,
        if cli.tm_frames { " of TM frames" } else { "" },
        cli.dest,
        match cli.rf_dest {
            Some(d) => format!(" (+{rf_sent} rf sample(s) → {d})"),
            None => String::new(),
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(line: &str) -> Vec<u8> {
        match parse_line(line).unwrap().unwrap().packet {
            PacketSpec::Raw(b) => b,
            other => panic!("expected raw packet, got {other:?}"),
        }
    }

    #[test]
    fn hex_line_round_trip() {
        let s = "00 2A C0 07 00 04 68 65 6C 6C 6F";
        assert_eq!(
            raw(s),
            vec![
                0x00, 0x2A, 0xC0, 0x07, 0x00, 0x04, b'h', b'e', b'l', b'l', b'o'
            ]
        );
    }

    #[test]
    fn jsonl_udp_hex() {
        let s = r#"{"udp_hex":"002AC007000468656C6C6F","note":"golden TM"}"#;
        assert_eq!(raw(s).len(), 11);
    }

    #[test]
    fn skip_comment_and_blank() {
        assert!(parse_line("").unwrap().is_none());
        assert!(parse_line("   ").unwrap().is_none());
        assert!(parse_line("# comment").unwrap().is_none());
    }

    #[test]
    fn rf_object_is_parsed_alongside_the_packet() {
        let s = r#"{"udp_hex":"002AC007000468656C6C6F",
            "rf":{"carrier_hz":437512000.5,"rx_power_dbm":-110.0,"az_deg":10.0,"el_deg":20.0}}"#
            .replace('\n', " ");
        let item = parse_line(&s).unwrap().unwrap();
        let rf = item.rf.unwrap();
        assert_eq!(rf.carrier_hz, Some(437_512_000.5));
        assert_eq!(rf.rx_power_dbm, Some(-110.0));
        assert_eq!((rf.az_deg, rf.el_deg), (Some(10.0), Some(20.0)));
        assert!(rf.t.is_none(), "arrival time is stamped by the gateway");
    }

    #[test]
    fn rf_with_lone_azimuth_or_bad_number_is_a_line_error() {
        let lone = r#"{"udp_hex":"002AC007000468656C6C6F","rf":{"az_deg":1.0}}"#;
        assert!(parse_line(lone).is_err());
        let bad = r#"{"udp_hex":"002AC007000468656C6C6F","rf":{"carrier_hz":"fast"}}"#;
        assert!(parse_line(bad).is_err());
        let not_obj = r#"{"udp_hex":"002AC007000468656C6C6F","rf":5}"#;
        assert!(parse_line(not_obj).is_err());
    }

    #[test]
    fn described_packet_is_encoded_with_a_cds_time_field() {
        let s =
            r#"{"apid":1968,"seq":7,"onboard_time":"2026-10-11T12:00:00Z","data_hex":"DEADBEEF"}"#;
        let item = parse_line(s).unwrap().unwrap();
        let pkt = item.packet.materialize(TimeFormat::Cds, 37).unwrap();
        // 6-byte primary header + 6-byte CDS + 4 user bytes; sec-hdr flag (bit 11) is set.
        assert_eq!(pkt.len(), 6 + 6 + 4);
        assert_eq!(u16::from_be_bytes([pkt[0], pkt[1]]) & 0x0800, 0x0800);
        assert_eq!(u16::from_be_bytes([pkt[0], pkt[1]]) & 0x07FF, 1968);
        assert_eq!(&pkt[12..], &[0xDE, 0xAD, 0xBE, 0xEF]);
        let t = chronus_gateway::time::decode_time(TimeFormat::Cds, &pkt[6..], 37)
            .unwrap()
            .unwrap();
        assert_eq!(t.to_rfc3339(), "2026-10-11T12:00:00+00:00");
    }

    #[test]
    fn now_is_stamped_at_send_time_and_cuc_is_supported() {
        let s = r#"{"apid":5,"onboard_time":"now","data_hex":"01"}"#;
        let item = parse_line(s).unwrap().unwrap();
        let before = Utc::now();
        let pkt = item.packet.materialize(TimeFormat::Cuc, 37).unwrap();
        let t = chronus_gateway::time::decode_time(TimeFormat::Cuc, &pkt[6..], 37)
            .unwrap()
            .unwrap();
        assert!((t - before).num_seconds().abs() <= 1);
    }

    #[test]
    fn described_packet_errors_are_specific() {
        for (s, needle) in [
            (
                r#"{"apid":5000,"onboard_time":"now","data_hex":"01"}"#,
                "apid",
            ),
            (r#"{"apid":5,"data_hex":"01"}"#, "onboard_time"),
            (r#"{"apid":5,"onboard_time":"now"}"#, "data_hex"),
            (
                r#"{"apid":5,"onboard_time":"yesterday","data_hex":"01"}"#,
                "RFC 3339",
            ),
            (r#"{"note":"nothing"}"#, "udp_hex"),
        ] {
            let e = format!("{:#}", parse_line(s).unwrap_err());
            assert!(e.contains(needle), "{s} -> {e}");
        }
    }

    #[test]
    fn short_raw_datagram_is_rejected_with_line_number() {
        let e = load_items("# c\n0102").unwrap_err().to_string();
        assert!(e.contains("line 2") && e.contains("too short"), "{e}");
        assert!(load_items("# only comments").is_err());
    }

    #[test]
    fn shipped_rf_lab_fixture_parses_and_carries_rf_on_every_line() {
        let text = include_str!("../../../demo/replay/fixtures/rf_lab.jsonl");
        let items = load_items(text).expect("fixture loads");
        assert_eq!(items.len(), 7);
        assert!(items.iter().all(|i| i.rf.is_some()));
        for item in &items {
            let pkt = item.packet.materialize(TimeFormat::Cds, 37).unwrap();
            assert_eq!(pkt.len(), 6 + 6 + 5, "header + CDS + HELLO");
        }
    }

    // ---- Tranche 3: --tm-frames ------------------------------------------------------------

    fn cli(args: &[&str]) -> Cli {
        let mut v = vec!["chronus-replay", "-f", "x"];
        v.extend_from_slice(args);
        Cli::parse_from(v)
    }

    #[test]
    fn tm_frames_wrap_is_decoded_back_by_the_gateway_deframer() {
        use chronus_gateway::config::{FramingConfig, FramingMode};
        use chronus_gateway::frames::{FramingStats, TmDeframer};
        let c = cli(&[
            "--tm-frames",
            "--frame-length",
            "64",
            "--asm",
            "--randomize",
            "--vcid",
            "2",
        ]);
        let (mut builder, opts) = c.framer().unwrap().expect("framing on");
        let packets: Vec<Vec<u8>> = (0..4)
            .map(|i| {
                chronus_gateway::encode_synthetic_tm(0x100, i, &vec![i as u8; 10 + 30 * i as usize])
            })
            .collect();
        let mut fc = FramingConfig::default();
        fc.mode = FramingMode::TmFrames;
        fc.frame_length = Some(64);
        fc.asm = true;
        fc.derandomize = true;
        let mut d = TmDeframer::new(&fc, std::sync::Arc::new(FramingStats::default()));
        let mut got = Vec::new();
        for p in &packets {
            let dgs = frame_packet(&mut builder, opts, p);
            assert!(
                dgs.iter()
                    .all(|dg| dg.len() == 68 && dg.starts_with(&chronus_gateway::frames::ASM))
            );
            for dg in dgs {
                d.push(&dg, |x| got.push((x.vcid, x.bytes.to_vec())));
            }
        }
        let want: Vec<_> = packets.into_iter().map(|p| (2u8, p)).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn tm_frame_options_are_checked() {
        assert!(cli(&[]).framer().unwrap().is_none());
        assert!(
            cli(&["--tm-frames", "--frame-length", "4"])
                .framer()
                .is_err()
        );
        assert!(cli(&["--tm-frames", "--scid", "2000"]).framer().is_err());
        assert!(cli(&["--tm-frames", "--vcid", "9"]).framer().is_err());
        let (mut b, opts) = cli(&["--tm-frames", "--no-fecf"])
            .framer()
            .unwrap()
            .unwrap();
        let dgs = frame_packet(
            &mut b,
            opts,
            &chronus_gateway::encode_synthetic_tm(1, 1, b"x"),
        );
        assert_eq!(dgs.len(), 1);
        assert_eq!(dgs[0].len(), 223);
        assert!(chronus_gateway::frames::parse_tm_frame(&dgs[0], false).is_ok());
    }

    #[test]
    fn shipped_tm_frames_fixture_loads_as_raw_datagrams() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../demo/replay/fixtures/tm_frames_lab.hex"
        );
        let items = load_items(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert!(items.len() >= 10);
        assert!(
            items
                .iter()
                .all(|i| matches!(&i.packet, PacketSpec::Raw(b) if b.len() >= 100))
        );
    }

    #[test]
    fn shipped_iss_pass_fixture_parses_with_a_doppler_fault_block() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../demo/replay/fixtures/iss_pass_lab.jsonl"
        );
        let items = load_items(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(items.len(), 330);
        assert_eq!(items.iter().filter(|i| i.rf.is_some()).count(), 25);
        assert!(
            items
                .iter()
                .all(|i| matches!(i.packet, PacketSpec::Timed { .. }))
        );
    }
}
