//! `chronus-soak` — drives a running gateway at a fixed packet rate with N WebSocket clients and
//! reports end-to-end latency percentiles, loss, and the memory trend (Tranche 5.2, Methodology D-055).
//!
//! ```text
//! chronus-soak --udp 127.0.0.1:7301 --http http://127.0.0.1:8080 \
//!     --rate 10000 --clients 50 --duration 24h --report soak.json --markdown soak.md
//! ```
//!
//! See `docs/SOAK.md` for the procedure and the recorded results.

use std::net::{SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chronus_evidence::soak::{
    LatencyHistogram, LatencySummary, ProbeFrame, RssSummary, RssTrend, parse_duration,
    probe_packet, prometheus_value, unix_nanos,
};
use clap::Parser;
use futures_util::StreamExt;
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

#[derive(Parser, Debug)]
#[command(version, about = "Soak / load driver for ChronusGateway-RS")]
struct Args {
    /// Gateway UDP ingest address.
    #[arg(long, default_value = "127.0.0.1:7301")]
    udp: SocketAddr,
    /// Gateway HTTP base URL (plain http; for a TLS front end run the tool on the host).
    #[arg(long, default_value = "http://127.0.0.1:8080")]
    http: String,
    /// API token, when the gateway requires one.
    #[arg(long, env = "CHRONUS_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// Packets per second.
    #[arg(long, default_value_t = 10_000)]
    rate: u32,
    /// WebSocket clients.
    #[arg(long, default_value_t = 50)]
    clients: u32,
    /// Run time (`90s`, `10m`, `24h`).
    #[arg(long, default_value = "60s", value_parser = parse_duration)]
    duration: Duration,
    /// Excluded from the memory verdict and latency figures while buffers fill.
    #[arg(long, default_value = "30s", value_parser = parse_duration)]
    warmup: Duration,
    /// Memory / counter sampling interval.
    #[arg(long, default_value = "10s", value_parser = parse_duration)]
    sample_every: Duration,
    /// Linux only: read memory from `/proc/<pid>/status` instead of the gateway's `/metrics`.
    #[arg(long)]
    pid: Option<u32>,
    /// APID of the probe packets.
    #[arg(long, default_value_t = 0x0AB, value_parser = parse_apid)]
    apid: u16,
    /// JSON report path.
    #[arg(long)]
    report: Option<PathBuf>,
    /// Markdown summary path.
    #[arg(long)]
    markdown: Option<PathBuf>,
    /// Fail (exit 1) if p99 latency exceeds this many milliseconds.
    #[arg(long)]
    max_p99_ms: Option<f64>,
    /// Fail if the second-half memory slope exceeds this many MiB per hour.
    #[arg(long)]
    max_rss_slope_mib_per_h: Option<f64>,
    /// Fail if more than this fraction of expected client messages is missing.
    #[arg(long)]
    max_loss: Option<f64>,
}

fn parse_apid(s: &str) -> Result<u16, String> {
    let v = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u16::from_str_radix(h, 16),
        None => s.parse(),
    }
    .map_err(|e| e.to_string())?;
    if v > 0x7FF {
        return Err("APID is 11 bits (0..=0x7FF)".into());
    }
    Ok(v)
}

#[derive(Default)]
struct ClientResult {
    hist: LatencyHistogram,
    received: u64,
    out_of_order: u64,
    connected: bool,
    error: Option<String>,
}

#[derive(Serialize)]
struct Report {
    tool: &'static str,
    started_unix_s: u64,
    duration_s: f64,
    warmup_s: f64,
    target_rate_pps: u32,
    achieved_rate_pps: f64,
    clients: u32,
    clients_connected: u32,
    packets_sent: u64,
    packets_sent_after_warmup: u64,
    messages_expected: u64,
    messages_received: u64,
    messages_received_total: u64,
    loss_fraction: f64,
    out_of_order: u64,
    latency: LatencySummary,
    rss: Option<RssSummary>,
    rss_samples: Vec<(f64, u64)>,
    gateway_counters: serde_json::Map<String, serde_json::Value>,
    client_errors: Vec<String>,
    verdict: Vec<String>,
    host: HostInfo,
}

#[derive(Serialize)]
struct HostInfo {
    os: &'static str,
    arch: &'static str,
    cpus: usize,
}

#[tokio::main]
async fn main() {
    if let Err(e) = run(Args::parse()).await {
        eprintln!("error: {e:#}");
        std::process::exit(2);
    }
}

async fn run(args: Args) -> Result<()> {
    if args.rate == 0 {
        bail!("--rate must be at least 1");
    }
    let ws_url = format!(
        "{}/telemetry/openmct",
        args.http
            .trim_end_matches('/')
            .replacen("http://", "ws://", 1)
            .replacen("https://", "wss://", 1)
    );
    let started_unix_s = unix_nanos() / 1_000_000_000;
    let t0 = Instant::now();
    let warmup_ns = u64::try_from(args.warmup.as_nanos()).unwrap_or(u64::MAX);
    let start_ns = unix_nanos();

    // 1. Clients first, so nothing the sender emits is missed.
    let stop = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::new();
    for _ in 0..args.clients {
        let url = ws_url.clone();
        let token = args.token.clone();
        let stop = Arc::clone(&stop);
        handles.push(tokio::spawn(async move {
            client(url, token, stop, start_ns + warmup_ns).await
        }));
    }
    // Let the upgrades finish before the first packet.
    tokio::time::sleep(Duration::from_millis(500 + 20 * u64::from(args.clients))).await;

    // 2. Sender on its own OS thread: a steady rate does not depend on the async scheduler.
    let sent = Arc::new(AtomicU64::new(0));
    let sent_after_warmup = Arc::new(AtomicU64::new(0));
    let sender = {
        let (sent, sent_aw) = (Arc::clone(&sent), Arc::clone(&sent_after_warmup));
        let (udp, rate, apid, dur) = (args.udp, args.rate, args.apid, args.duration);
        let warm_until = start_ns + warmup_ns;
        std::thread::spawn(move || -> Result<f64> {
            let sock = UdpSocket::bind(if udp.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" })?;
            sock.connect(udp)?;
            let begin = Instant::now();
            let mut seq = 0u64;
            while begin.elapsed() < dur {
                let due = (begin.elapsed().as_secs_f64() * f64::from(rate)) as u64;
                while seq < due {
                    let now = unix_nanos();
                    // A full socket buffer is load the gateway did not see; count only real sends.
                    if sock.send(&probe_packet(apid, seq, now)).is_ok() {
                        sent.fetch_add(1, Ordering::Relaxed);
                        if now >= warm_until {
                            sent_aw.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    seq += 1;
                }
                std::thread::sleep(Duration::from_micros(500));
            }
            Ok(seq as f64 / begin.elapsed().as_secs_f64())
        })
    };

    // 3. Sampler: memory and the gateway's own drop counters.
    let mut rss = RssTrend::default();
    let mut counters = serde_json::Map::new();
    let mut tick = tokio::time::interval(args.sample_every);
    let deadline = t0 + args.duration + Duration::from_millis(500 + 20 * u64::from(args.clients));
    let mut last_print = Instant::now();
    loop {
        tick.tick().await;
        let t = t0.elapsed().as_secs_f64();
        let metrics = scrape(&args.http, args.token.as_deref()).await.ok();
        let bytes = match args.pid {
            Some(pid) => proc_rss(pid),
            None => metrics
                .as_deref()
                .and_then(|m| prometheus_value(m, "process_resident_memory_bytes"))
                .map(|v| v as u64),
        };
        if let Some(b) = bytes {
            rss.push(t, b);
        }
        if let Some(m) = &metrics {
            counters = gateway_counters(m);
        }
        if last_print.elapsed() >= Duration::from_secs(60) || Instant::now() >= deadline {
            eprintln!(
                "[{:>7.0}s] sent {} rss {} MiB",
                t,
                sent.load(Ordering::Relaxed),
                bytes.map_or("?".into(), |b| format!("{:.1}", b as f64 / 1_048_576.0))
            );
            last_print = Instant::now();
        }
        if Instant::now() >= deadline {
            break;
        }
    }
    let achieved = sender
        .join()
        .map_err(|_| anyhow::anyhow!("sender thread panicked"))??;

    // 4. Drain, then stop the clients.
    tokio::time::sleep(Duration::from_secs(2)).await;
    if let Ok(m) = scrape(&args.http, args.token.as_deref()).await {
        counters = gateway_counters(&m);
    }
    stop.store(true, Ordering::Relaxed);
    let mut hist = LatencyHistogram::default();
    let (mut received, mut ooo, mut connected) = (0u64, 0u64, 0u32);
    let mut errors = Vec::new();
    for h in handles {
        let r = h.await.unwrap_or_default();
        hist.merge(&r.hist);
        received += r.received;
        ooo += r.out_of_order;
        connected += u32::from(r.connected);
        if let Some(e) = r.error {
            errors.push(e);
        }
    }
    errors.sort();
    errors.dedup();

    let packets_sent = sent.load(Ordering::Relaxed);
    let sent_aw = sent_after_warmup.load(Ordering::Relaxed);
    let expected = sent_aw * u64::from(connected);
    let loss = if expected == 0 {
        0.0
    } else {
        1.0 - (hist.len().min(expected) as f64 / expected as f64)
    };
    let latency = hist.summary();
    let rss_summary = rss.summary(args.warmup.as_secs_f64());

    let mut verdict = Vec::new();
    if connected < args.clients {
        verdict.push(format!(
            "FAIL: only {connected} of {} clients connected",
            args.clients
        ));
    }
    if let (Some(max), Some(p99)) = (args.max_p99_ms, latency.p99_ms)
        && p99 > max
    {
        verdict.push(format!("FAIL: p99 {p99:.2} ms > {max} ms"));
    }
    if let Some(max) = args.max_loss
        && loss > max
    {
        verdict.push(format!("FAIL: loss {loss:.5} > {max}"));
    }
    if let Some(max) = args.max_rss_slope_mib_per_h {
        match &rss_summary {
            Some(s) if s.second_half_slope_mib_per_h > max => verdict.push(format!(
                "FAIL: memory grows {:.2} MiB/h > {max}",
                s.second_half_slope_mib_per_h
            )),
            None => verdict.push("FAIL: no memory samples (see --pid)".into()),
            _ => {}
        }
    }
    if verdict.is_empty() {
        verdict.push("PASS".into());
    }

    let report = Report {
        tool: concat!("chronus-soak ", env!("CARGO_PKG_VERSION")),
        started_unix_s,
        duration_s: args.duration.as_secs_f64(),
        warmup_s: args.warmup.as_secs_f64(),
        target_rate_pps: args.rate,
        achieved_rate_pps: achieved,
        clients: args.clients,
        clients_connected: connected,
        packets_sent,
        packets_sent_after_warmup: sent_aw,
        messages_expected: expected,
        messages_received: hist.len(),
        messages_received_total: received,
        loss_fraction: loss,
        out_of_order: ooo,
        latency,
        rss: rss_summary,
        rss_samples: rss.samples.clone(),
        gateway_counters: counters,
        client_errors: errors,
        verdict,
        host: HostInfo {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            cpus: std::thread::available_parallelism().map_or(1, usize::from),
        },
    };
    let md = markdown(&report);
    println!("{md}");
    if let Some(p) = &args.report {
        std::fs::write(p, serde_json::to_string_pretty(&report)?)
            .with_context(|| format!("writing {}", p.display()))?;
    }
    if let Some(p) = &args.markdown {
        std::fs::write(p, &md).with_context(|| format!("writing {}", p.display()))?;
    }
    if report.verdict.iter().any(|v| v.starts_with("FAIL")) {
        std::process::exit(1);
    }
    Ok(())
}

async fn client(
    url: String,
    token: Option<String>,
    stop: Arc<AtomicBool>,
    count_from_ns: u64,
) -> ClientResult {
    let mut r = ClientResult::default();
    let mut req = match url.as_str().into_client_request() {
        Ok(q) => q,
        Err(e) => {
            r.error = Some(format!("bad WebSocket URL: {e}"));
            return r;
        }
    };
    if let Some(t) = token
        && let Ok(v) = format!("Bearer {t}").parse()
    {
        req.headers_mut().insert("authorization", v);
    }
    let ws = match tokio_tungstenite::connect_async(req).await {
        Ok((ws, _)) => ws,
        Err(e) => {
            r.error = Some(format!("connect: {e}"));
            return r;
        }
    };
    r.connected = true;
    let (_sink, mut stream) = ws.split();
    let mut last_seq: Option<u64> = None;
    while !stop.load(Ordering::Relaxed) {
        let msg = match tokio::time::timeout(Duration::from_millis(250), stream.next()).await {
            Err(_) => continue,
            Ok(None) => {
                r.error = Some("gateway closed the WebSocket".into());
                break;
            }
            Ok(Some(Err(e))) => {
                r.error = Some(format!("WebSocket: {e}"));
                break;
            }
            Ok(Some(Ok(m))) => m,
        };
        let Message::Text(text) = msg else { continue };
        let now = unix_nanos();
        let Some(p) = ProbeFrame::decode(text.as_str()) else {
            continue;
        };
        r.received += 1;
        if last_seq.is_some_and(|l| p.seq <= l) {
            r.out_of_order += 1;
        }
        last_seq = Some(last_seq.map_or(p.seq, |l| l.max(p.seq)));
        if p.sent_ns >= count_from_ns {
            r.hist
                .record(Duration::from_nanos(now.saturating_sub(p.sent_ns)));
        }
    }
    r
}

/// `GET /metrics` over plain HTTP/1.1 (the tool's only HTTP need; no client library).
async fn scrape(base: &str, token: Option<&str>) -> Result<String> {
    let rest = base
        .strip_prefix("http://")
        .context("--http must be an http:// URL")?;
    let host = rest.split('/').next().unwrap_or(rest);
    let mut s = tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect(host))
        .await??;
    let auth = token.map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
    s.write_all(
        format!("GET /metrics HTTP/1.1\r\nHost: {host}\r\n{auth}Connection: close\r\n\r\n")
            .as_bytes(),
    )
    .await?;
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut buf)).await??;
    let text = String::from_utf8_lossy(&buf);
    let (head, body) = text.split_once("\r\n\r\n").context("malformed response")?;
    if !head.starts_with("HTTP/1.1 200") {
        bail!("{}", head.lines().next().unwrap_or("no status"));
    }
    Ok(body.to_string())
}

fn proc_rss(pid: u32) -> Option<u64> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let kb: u64 = s
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))?
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .ok()?;
    Some(kb * 1024)
}

fn gateway_counters(metrics: &str) -> serde_json::Map<String, serde_json::Value> {
    const NAMES: &[&str] = &[
        "chronus_ingest_frames_received_total",
        "chronus_ingest_oversized_dropped_total",
        "chronus_ingest_recv_errors_total",
        "chronus_gateway_telemetry_frames_emitted_total",
        "chronus_gateway_telemetry_parse_errors_total",
        "chronus_gateway_ws_messages_sent_total",
        "chronus_pipeline_datagrams_lagged_total",
        "chronus_pipeline_ws_messages_dropped_total",
        "chronus_history_len",
    ];
    NAMES
        .iter()
        .filter_map(|n| Some(((*n).to_string(), prometheus_value(metrics, n)?.into())))
        .collect()
}

fn markdown(r: &Report) -> String {
    let ms = |v: Option<f64>| v.map_or("–".into(), |x| format!("{x:.2}"));
    let mut s = String::new();
    s += &format!(
        "## Soak run: {} pkt/s, {} WebSocket clients, {:.0} s\n\n",
        r.target_rate_pps, r.clients, r.duration_s
    );
    s += "| | |\n|---|---|\n";
    s += &format!(
        "| host | {} {} ({} CPUs) |\n",
        r.host.os, r.host.arch, r.host.cpus
    );
    s += &format!(
        "| packets sent | {} ({:.0} pkt/s achieved) |\n",
        r.packets_sent, r.achieved_rate_pps
    );
    s += &format!(
        "| clients connected | {} / {} |\n",
        r.clients_connected, r.clients
    );
    s += &format!(
        "| messages received | {} of {} expected after warm-up (loss {:.4} %) |\n",
        r.messages_received,
        r.messages_expected,
        r.loss_fraction * 100.0
    );
    s += &format!(
        "| latency p50 / p90 / p99 / p99.9 / max (ms) | {} / {} / {} / {} / {} |\n",
        ms(r.latency.p50_ms),
        ms(r.latency.p90_ms),
        ms(r.latency.p99_ms),
        ms(r.latency.p999_ms),
        ms(r.latency.max_ms)
    );
    if let Some(m) = &r.rss {
        s += &format!(
            "| RSS after warm-up / peak / end (MiB) | {:.1} / {:.1} / {:.1} |\n",
            m.after_warmup_mib, m.peak_mib, m.last_mib
        );
        s += &format!(
            "| RSS slope, second half (MiB/h) | {:+.2} |\n",
            m.second_half_slope_mib_per_h
        );
    }
    for (k, v) in &r.gateway_counters {
        s += &format!("| `{k}` | {v} |\n");
    }
    s += &format!("| verdict | {} |\n", r.verdict.join("; "));
    if !r.client_errors.is_empty() {
        s += &format!("\nClient errors: {}\n", r.client_errors.join("; "));
    }
    s
}
