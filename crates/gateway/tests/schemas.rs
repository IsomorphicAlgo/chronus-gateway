//! Tranche 3.3: the published JSON Schemas (`crates/gateway/schemas/`) describe what the gateway
//! actually emits. Each schema must itself be valid, real output must validate against it, and
//! obviously wrong documents must not.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::Request;
use chrono::Utc;
use chronus_gateway::config::StationConfig;
use chronus_gateway::ingest::{IngestStats, RawFrame};
use chronus_gateway::metrics::GatewayMetrics;
use chronus_gateway::propagator::{TrackingProvider, propagator_from_station};
use chronus_gateway::rf::RfSampleV1;
use chronus_gateway::state::SharedGateway;
use chronus_gateway::time::{TimeFormat, encode_cds};
use chronus_gateway::{encode_synthetic_tm, encode_synthetic_tm_with_time, http};
use jsonschema::{Registry, Validator};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tower::ServiceExt;

const REALTIME_ID: &str = "https://github.com/IsomorphicAlgo/chronus-gateway/blob/master/crates/gateway/schemas/openmct.realtime.v1.schema.json";

fn schema(name: &str) -> Value {
    let doc = http::SCHEMAS
        .iter()
        .find(|(n, _)| *n == name)
        .unwrap_or_else(|| panic!("schema {name} is published"))
        .1;
    serde_json::from_str(doc).expect("schema is JSON")
}

fn validator(name: &str) -> Validator {
    let registry = Registry::new()
        .add(REALTIME_ID, schema("openmct.realtime.v1"))
        .expect("add")
        .prepare()
        .expect("prepare");
    jsonschema::options()
        .with_registry(&registry)
        .should_validate_formats(true)
        .build(&schema(name))
        .unwrap_or_else(|e| panic!("{name} compiles: {e}"))
}

fn assert_valid(v: &Validator, doc: &Value) {
    let errors: Vec<String> = v.iter_errors(doc).map(|e| e.to_string()).collect();
    assert!(errors.is_empty(), "{errors:?}\n{doc:#}");
}

fn gateway(tracking: bool, configure: impl FnOnce(&mut StationConfig)) -> Arc<SharedGateway> {
    let mut station = StationConfig::default();
    configure(&mut station);
    let tracking = tracking.then(|| {
        Arc::new(TrackingProvider::new(
            propagator_from_station(&station).unwrap(),
            0,
        ))
    });
    let (tx, _rx) = broadcast::channel(8);
    Arc::new(SharedGateway::new(
        tx,
        Arc::new(IngestStats::default()),
        Arc::new(GatewayMetrics::default()),
        Arc::new(station),
        tracking,
    ))
}

fn raw(bytes: &[u8]) -> RawFrame {
    RawFrame {
        bytes: Arc::from(bytes),
        received_at: Utc::now(),
        source: SocketAddr::from(([127, 0, 0, 1], 5)),
    }
}

#[test]
fn every_published_schema_is_a_valid_json_schema() {
    for (name, doc) in http::SCHEMAS {
        let v: Value = serde_json::from_str(doc).unwrap();
        assert!(jsonschema::meta::is_valid(&v), "{name}");
        assert_eq!(v["title"], *name);
        assert_eq!(
            v["properties"]["chronus_schema"]["const"], *name,
            "{name} pins its own id"
        );
    }
}

#[test]
fn realtime_messages_validate_with_and_without_physics_and_time() {
    let v = validator("openmct.realtime.v1");
    // Physics on, CDS time decoded.
    let g = gateway(true, |s| s.time.format = TimeFormat::Cds);
    let pkt = encode_synthetic_tm_with_time(9, 3, &encode_cds(Utc::now()).unwrap(), b"hi");
    let msg: Value = serde_json::from_str(&http::process_frame(&g, &raw(&pkt)).unwrap()).unwrap();
    assert!(msg["onboard_time"].is_string());
    assert_valid(&v, &msg);
    // No tracking: physics fields are null.
    let g = gateway(false, |_| {});
    let msg: Value = serde_json::from_str(
        &http::process_frame(&g, &raw(&encode_synthetic_tm(1, 1, b"x"))).unwrap(),
    )
    .unwrap();
    assert!(msg["elevation_deg"].is_null());
    assert_valid(&v, &msg);
    // ...and with a virtual channel, via the TM pipeline.
    let mut tagged = msg.clone();
    tagged["vcid"] = json!(3);
    assert_valid(&v, &tagged);

    for (field, bad) in [
        ("apid", json!(5000)),
        ("physics_flags", json!(-1)),
        ("received_at", json!("yesterday")),
        ("vcid", json!(8)),
        ("chronus_schema", json!("openmct.realtime.v2")),
    ] {
        let mut wrong = msg.clone();
        wrong[field] = bad;
        assert!(!v.is_valid(&wrong), "{field} should be rejected");
    }
    let mut missing = msg;
    missing.as_object_mut().unwrap().remove("apid");
    assert!(!v.is_valid(&missing));
}

#[test]
fn rf_datagrams_validate_and_half_a_pointing_measurement_does_not() {
    let v = validator("chronus.rf.v1");
    for s in [
        RfSampleV1::new(),
        RfSampleV1::new().with_carrier_hz(437.5e6),
        RfSampleV1::new()
            .with_carrier_hz(437.5e6)
            .with_rx_power_dbm(-110.0)
            .with_pointing_deg(120.0, 33.0)
            .with_time(Utc::now()),
    ] {
        let doc: Value = serde_json::from_slice(&s.to_datagram()).unwrap();
        assert_valid(&v, &doc);
    }
    assert!(!v.is_valid(&json!({"chronus_schema": "chronus.rf.v1", "az_deg": 10.0})));
    assert!(!v.is_valid(&json!({"chronus_schema": "chronus.rf.v2"})));
    assert!(!v.is_valid(&json!({"chronus_schema": "chronus.rf.v1", "carrier_hz": "fast"})));
}

#[tokio::test]
async fn history_response_and_dictionary_validate() {
    let g = gateway(true, |_| {});
    for i in 0..3 {
        chronus_gateway::pipeline::handle_datagram(
            &g,
            None,
            &raw(&encode_synthetic_tm(0x100, i, b"x")),
        );
    }
    let res = http::router(Arc::clone(&g))
        .oneshot(
            Request::builder()
                .uri("/api/v1/chronus/history?limit=2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body: Value =
        serde_json::from_slice(&to_bytes(res.into_body(), 1 << 20).await.unwrap()).unwrap();
    assert_eq!(body["count"], 2);
    let v = validator("chronus.history.v1");
    assert_valid(&v, &body);
    // A message inside the page is checked against the referenced realtime schema.
    let mut broken = body.clone();
    broken["messages"][0]["apid"] = json!("x");
    assert!(!v.is_valid(&broken));

    let d = chronus_gateway::openmct::dictionary(&g.station);
    assert_valid(&validator("chronus.openmct.dictionary.v1"), &d);
}
