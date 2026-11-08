//! Prometheus text exposition (`GET /metrics`, Tranche 4).
//!
//! The same counters as `GET /api/v1/chronus/metrics`, in the Prometheus text format (0.0.4), so a
//! standard scraper can alert on them. Names are `chronus_<group>_<field>`; counters end in
//! `_total`. The groups are generated from the JSON snapshots, so a counter added to a snapshot
//! appears here without further work. The data-age gauges are in seconds, as Prometheus
//! recommends.
//!
//! The standard `process_resident_memory_bytes` gauge is included on Linux, Windows and macOS,
//! so memory can be watched without access to the host; the soak tool (`chronus-soak`,
//! 1.0.0-rc) uses it.
//!
//! `/metrics` sits behind the same API token as everything else; configure the scraper with
//! `authorization: { credentials: <token> }`.

use std::fmt::Write;

use serde::Serialize;
use serde_json::Value;

use crate::diagnose::OverallStatus;
use crate::state::SharedGateway;

/// Fields that are levels rather than ever-increasing counts.
const GAUGES: &[&str] = &[
    "ws_clients_connected",
    "enabled",
    "auth_enabled",
    "len",
    "capacity",
    "persistent",
];

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn emit(out: &mut String, name: &str, kind: &str, help: &str, value: f64) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} {kind}");
    let _ = writeln!(out, "{name} {value}");
}

/// Emits every numeric or boolean field of `snapshot` under `chronus_<group>_`.
fn group(out: &mut String, group: &str, snapshot: &impl Serialize) {
    let Ok(Value::Object(map)) = serde_json::to_value(snapshot) else {
        return;
    };
    for (field, v) in &map {
        let value = match v {
            Value::Number(n) => n.as_f64(),
            Value::Bool(b) => Some(f64::from(u8::from(*b))),
            _ => None,
        };
        let Some(value) = value.filter(|v| v.is_finite()) else {
            continue;
        };
        let base = format!("chronus_{}_{}", sanitize(group), sanitize(field));
        if GAUGES.contains(&field.as_str()) {
            emit(out, &base, "gauge", &format!("{group}.{field}"), value);
        } else {
            emit(
                out,
                &format!("{base}_total"),
                "counter",
                &format!("{group}.{field}"),
                value,
            );
        }
    }
}

/// Renders all gateway metrics in the Prometheus text format.
#[must_use]
pub fn render(state: &SharedGateway) -> String {
    let mut out = String::with_capacity(8 * 1024);
    let now = chrono::Utc::now();
    let _ = writeln!(
        out,
        "# HELP chronus_build_info Gateway version.\n# TYPE chronus_build_info gauge\nchronus_build_info{{version=\"{}\"}} 1",
        env!("CARGO_PKG_VERSION")
    );

    let health = state.health_report();
    let level = match health.status {
        OverallStatus::Ok => 0.0,
        OverallStatus::Waiting => 1.0,
        OverallStatus::Attention => 2.0,
        _ => 3.0,
    };
    emit(
        &mut out,
        "chronus_health_status",
        "gauge",
        "Overall /status: 0 ok, 1 waiting, 2 attention, 3 problem.",
        level,
    );

    group(&mut out, "ingest", &state.ingest_stats.snapshot_struct());
    group(&mut out, "gateway", &state.gateway_metrics.snapshot());
    group(&mut out, "rf", &state.rf.snapshot());
    group(
        &mut out,
        "framing",
        &state.framing.snapshot(state.station.framing.mode),
    );
    group(&mut out, "history", &state.history.snapshot());
    group(&mut out, "pipeline", &state.pipeline.snapshot());
    group(
        &mut out,
        "security",
        &state.security.snapshot(state.auth.is_enabled()),
    );

    let d = state.data.snapshot(now);
    if let Some(days) = d.tle_age_days {
        emit(
            &mut out,
            "chronus_data_tle_age_seconds",
            "gauge",
            "Age of the TLE in use (negative: dated in the future).",
            days * 86_400.0,
        );
    }
    if let Some(h) = d.oem_hours_remaining {
        emit(
            &mut out,
            "chronus_data_oem_remaining_seconds",
            "gauge",
            "OEM coverage left from now (negative: ended).",
            h * 3_600.0,
        );
    }
    if let Some(day) = d.eop_file_last_day
        && let Some(end) = day.succ_opt().and_then(|n| n.and_hms_opt(0, 0, 0))
    {
        emit(
            &mut out,
            "chronus_data_eop_remaining_seconds",
            "gauge",
            "Earth orientation file coverage left from now (negative: ended).",
            (end.and_utc() - now).num_seconds() as f64,
        );
    }
    emit(
        &mut out,
        "chronus_data_ut1_minus_utc_seconds",
        "gauge",
        "UT1-UTC in use.",
        d.ut1_minus_utc_s,
    );
    emit(
        &mut out,
        "chronus_data_reloads_total",
        "counter",
        "Successful data reloads.",
        d.reloads as f64,
    );
    emit(
        &mut out,
        "chronus_data_reload_errors_total",
        "counter",
        "Failed data reloads.",
        d.reload_errors as f64,
    );
    emit(
        &mut out,
        "chronus_data_fetch_errors_total",
        "counter",
        "Failed data fetches.",
        d.fetch_errors as f64,
    );
    if let Some(rss) = resident_memory_bytes() {
        emit(
            &mut out,
            "process_resident_memory_bytes",
            "gauge",
            "Resident memory size in bytes.",
            rss as f64,
        );
    }
    out
}

/// Resident memory of this process in bytes (Linux, Windows, macOS; `None` elsewhere).
///
/// Linux reads `/proc/self/statm` with the real page size; Windows reports the working set.
#[must_use]
pub fn resident_memory_bytes() -> Option<u64> {
    memory_stats::memory_stats().map(|m| m.physical_mem as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StationConfig;
    use crate::ingest::IngestStats;
    use crate::metrics::GatewayMetrics;
    use std::sync::Arc;

    #[test]
    fn exposition_is_well_formed_and_names_are_unique() {
        let (tx, _rx) = tokio::sync::broadcast::channel(4);
        let g = SharedGateway::new(
            tx,
            Arc::new(IngestStats::default()),
            Arc::new(GatewayMetrics::default()),
            Arc::new(StationConfig::default()),
            None,
        );
        g.gateway_metrics
            .telemetry_frames_emitted
            .store(7, std::sync::atomic::Ordering::Relaxed);
        let text = render(&g);
        assert!(text.contains("chronus_gateway_telemetry_frames_emitted_total 7"));
        assert!(text.contains("# TYPE chronus_gateway_ws_clients_connected gauge"));
        assert!(text.contains("chronus_history_capacity 10000"));
        assert!(text.contains("chronus_security_auth_enabled 0"));
        assert!(text.contains("chronus_build_info{version="));
        if cfg!(any(
            target_os = "linux",
            target_os = "windows",
            target_os = "macos"
        )) {
            assert!(resident_memory_bytes().unwrap() > 1_000_000);
            assert!(text.contains("# TYPE process_resident_memory_bytes gauge"));
        }
        let mut names = std::collections::HashSet::new();
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("# TYPE ") {
                let name = rest.split(' ').next().unwrap();
                assert!(names.insert(name.to_string()), "duplicate {name}");
                assert!(
                    name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
                    "{name}"
                );
            } else if !line.starts_with('#') {
                let (name, value) = line.rsplit_once(' ').unwrap();
                assert!(value.parse::<f64>().is_ok(), "{line}");
                assert!(!name.is_empty());
            }
        }
    }
}
