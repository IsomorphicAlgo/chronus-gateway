//! Criterion benchmarks for CCSDS parse + physics validation hot paths (Milestone 6).

use std::net::SocketAddr;
use std::sync::Arc;

use chrono::Utc;
use chronus_gateway::ccsds::{self, CCSDS_PRIMARY_HEADER_LEN};
use chronus_gateway::ingest::RawFrame;
use chronus_gateway::propagator::TrackingState;
use chronus_gateway::validate::{apply_physics_validation, RfMetadata};
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn tm_bytes(apid: u16, seq: u16, payload: &[u8]) -> Vec<u8> {
    assert!(!payload.is_empty());
    let version = 0u16;
    let ptype = 0u16; // TM
    let sec_hdr = 0u16;
    let word1 = (version << 13) | (ptype << 12) | (sec_hdr << 11) | (apid & 0x07FF);
    let seq_flags = 0b11u16;
    let word2 = (seq_flags << 14) | (seq & 0x3FFF);
    let data_len = (payload.len() - 1) as u16;
    let mut v = Vec::with_capacity(CCSDS_PRIMARY_HEADER_LEN + payload.len());
    v.extend_from_slice(&word1.to_be_bytes());
    v.extend_from_slice(&word2.to_be_bytes());
    v.extend_from_slice(&data_len.to_be_bytes());
    v.extend_from_slice(payload);
    v
}

fn sample_frame() -> RawFrame {
    RawFrame {
        bytes: Arc::from(tm_bytes(0x2A, 7, b"benchmark-payload").into_boxed_slice()),
        received_at: Utc::now(),
        source: SocketAddr::from(([127, 0, 0, 1], 7301)),
    }
}

fn bench_parse_telemetry(c: &mut Criterion) {
    let frame = sample_frame();
    c.bench_function("parse_telemetry", |b| {
        b.iter(|| black_box(ccsds::parse_telemetry(black_box(&frame))))
    });
}

fn bench_apply_physics_validation(c: &mut Criterion) {
    let frame = sample_frame();
    let tm = ccsds::parse_telemetry(&frame).expect("valid TM");
    let state = TrackingState {
        azimuth_deg: 180.0,
        elevation_deg: 45.0,
        range_km: 450.0,
        range_rate_km_s: -7.2,
    };
    let nominal = 437_500_000.0_f64;
    c.bench_function("apply_physics_validation", |b| {
        b.iter(|| {
            let mut t = tm.clone();
            apply_physics_validation(
                black_box(&mut t),
                black_box(&state),
                nominal,
                RfMetadata::default(),
                150.0,
                0.0,
            );
            black_box(t.physics_flags);
        })
    });
}

criterion_group!(benches, bench_parse_telemetry, bench_apply_physics_validation);
criterion_main!(benches);
