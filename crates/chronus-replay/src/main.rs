//! Replay synthetic CCSDS TM **UDP datagrams** from a text fixture (Showcase S3).
//!
//! Each non-empty, non-`#` line is either:
//! - **Hex:** whitespace-separated or contiguous hex (one full Space Packet per line).
//! - **JSONL:** a JSON object with a **`udp_hex`** key (contiguous hex, one datagram per line).
//!
//! All payloads must be **synthetic / lab-generated** per project compliance (`AGENTS.md`).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::Parser;
use tokio::fs::read_to_string;
use tokio::net::UdpSocket;
use tokio::time::sleep;

#[derive(Parser, Debug)]
#[command(name = "chronus-replay", version, about = "Replay UDP CCSDS TM datagrams from a hex/JSONL fixture")]
struct Cli {
    /// Fixture file (hex lines or JSONL with `udp_hex` per line).
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
        let b = u8::from_str_radix(pair, 16).with_context(|| format!("invalid hex pair `{pair}`"))?;
        out.push(b);
    }
    Ok(out)
}

/// One logical line → one UDP payload, or `None` to skip (blank / comment).
fn line_to_datagram(line: &str) -> Result<Option<Vec<u8>>> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return Ok(None);
    }
    if line.starts_with('{') {
        let v: serde_json::Value = serde_json::from_str(line).context("invalid JSON line")?;
        let hex = v
            .get("udp_hex")
            .and_then(|x| x.as_str())
            .context("JSONL line requires string field `udp_hex`")?;
        return Ok(Some(parse_hex_bytes(hex)?));
    }
    Ok(Some(parse_hex_bytes(line)?))
}

fn load_datagrams(text: &str) -> Result<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let n = idx + 1;
        match line_to_datagram(line) {
            Ok(None) => {}
            Ok(Some(pkt)) => {
                if pkt.len() < 7 {
                    bail!("line {n}: datagram too short for a CCSDS primary header ({})", pkt.len());
                }
                out.push(pkt);
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
    let packets = load_datagrams(&text)?;

    let sock = UdpSocket::bind("0.0.0.0:0")
        .await
        .context("bind ephemeral UDP")?;
    let delay = Duration::from_millis(cli.delay_ms);

    for r in 0..cli.repeat {
        if cli.repeat > 1 {
            eprintln!("chronus-replay: repeat {}/{}", r + 1, cli.repeat);
        }
        for (i, pkt) in packets.iter().enumerate() {
            sock.send_to(pkt, cli.dest)
                .await
                .with_context(|| format!("send datagram {}", i + 1))?;
            if cli.delay_ms > 0 {
                sleep(delay).await;
            }
        }
    }

    eprintln!(
        "chronus-replay: sent {} datagram(s) × {} repeat(s) → {}",
        packets.len(),
        cli.repeat,
        cli.dest
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_line_round_trip() {
        let s = "00 2A C0 07 00 04 68 65 6C 6C 6F";
        let v = line_to_datagram(s).unwrap().unwrap();
        assert_eq!(v, vec![0x00, 0x2A, 0xC0, 0x07, 0x00, 0x04, b'h', b'e', b'l', b'l', b'o']);
    }

    #[test]
    fn jsonl_udp_hex() {
        let s = r#"{"udp_hex":"002AC007000468656C6C6F","note":"golden TM"}"#;
        let v = line_to_datagram(s).unwrap().unwrap();
        assert_eq!(v.len(), 11);
    }

    #[test]
    fn skip_comment_and_blank() {
        assert!(line_to_datagram("").unwrap().is_none());
        assert!(line_to_datagram("   ").unwrap().is_none());
        assert!(line_to_datagram("# comment").unwrap().is_none());
    }
}
