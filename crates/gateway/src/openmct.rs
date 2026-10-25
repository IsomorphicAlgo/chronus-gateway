//! Open MCT telemetry dictionary (Tranche 3, `Methodology.md` D-049).
//!
//! Open MCT needs to know which telemetry points exist before it can list, plot or request history
//! for them. This module holds the **single table** of points the gateway publishes. Each point is
//! one field of the `openmct.realtime.v1` message, so the dictionary served at
//! `GET /api/v1/chronus/openmct/dictionary`, the per-point history at
//! `GET /api/v1/chronus/openmct/history/{key}` and the realtime projection in the Open MCT plugin
//! (`demo/openmct/`) are all generated from the same rows and cannot drift apart.
//!
//! The dictionary follows the shape of the Open MCT tutorial dictionary (`measurements`, each with
//! `values` carrying `hints.range` / `hints.domain`), plus a `chronus_field` per measurement naming
//! the message field it comes from.

use serde::Serialize;
use serde_json::{Value, json};

use crate::config::{EphemerisSource, StationConfig};

/// Schema id of the dictionary document.
pub const DICTIONARY_SCHEMA_V1: &str = "chronus.openmct.dictionary.v1";

/// How a point's value is formatted in Open MCT.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum PointFormat {
    /// An integer counter or identifier.
    Integer,
    /// A floating-point measurement.
    Float,
}

/// One telemetry point: a numeric field of the `openmct.realtime.v1` message.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct PointDef {
    /// Open MCT identifier key, e.g. `chronus.physics.elevation_deg`.
    pub key: &'static str,
    /// Human-readable name.
    pub name: &'static str,
    /// Field of the realtime JSON message the value is read from.
    pub field: &'static str,
    /// Units, when the value has any.
    pub units: Option<&'static str>,
    /// Display format.
    pub format: PointFormat,
    /// Whether any non-zero value is an alarm (drives Open MCT limits).
    pub alarm_when_nonzero: bool,
}

const fn point(
    key: &'static str,
    name: &'static str,
    field: &'static str,
    units: Option<&'static str>,
    format: PointFormat,
    alarm_when_nonzero: bool,
) -> PointDef {
    PointDef {
        key,
        name,
        field,
        units,
        format,
        alarm_when_nonzero,
    }
}

/// Every point the gateway publishes, in dictionary order.
pub const POINTS: &[PointDef] = &[
    point(
        "chronus.tm.physics_flags",
        "Physics alarm flags",
        "physics_flags",
        None,
        PointFormat::Integer,
        true,
    ),
    point(
        "chronus.tm.physics_flags_v2",
        "Time alarm flags",
        "physics_flags_v2",
        None,
        PointFormat::Integer,
        true,
    ),
    point(
        "chronus.physics.elevation_deg",
        "Elevation",
        "elevation_deg",
        Some("deg"),
        PointFormat::Float,
        false,
    ),
    point(
        "chronus.physics.azimuth_deg",
        "Azimuth",
        "azimuth_deg",
        Some("deg"),
        PointFormat::Float,
        false,
    ),
    point(
        "chronus.physics.range_km",
        "Slant range",
        "range_km",
        Some("km"),
        PointFormat::Float,
        false,
    ),
    point(
        "chronus.physics.range_rate_km_s",
        "Range rate",
        "range_rate_km_s",
        Some("km/s"),
        PointFormat::Float,
        false,
    ),
    point(
        "chronus.physics.apparent_elevation_deg",
        "Apparent elevation",
        "apparent_elevation_deg",
        Some("deg"),
        PointFormat::Float,
        false,
    ),
    point(
        "chronus.physics.doppler_tolerance_hz",
        "Doppler tolerance",
        "doppler_tolerance_hz",
        Some("Hz"),
        PointFormat::Float,
        false,
    ),
    point(
        "chronus.rf.matched",
        "Receiver data matched",
        "rf_matched",
        None,
        PointFormat::Integer,
        false,
    ),
    point(
        "chronus.tm.apid",
        "APID",
        "apid",
        None,
        PointFormat::Integer,
        false,
    ),
    point(
        "chronus.tm.seq_count",
        "Sequence count",
        "seq_count",
        None,
        PointFormat::Integer,
        false,
    ),
    point(
        "chronus.tm.vcid",
        "Virtual channel",
        "vcid",
        None,
        PointFormat::Integer,
        false,
    ),
];

/// Index of the point with this key in [`POINTS`].
#[must_use]
pub fn point_index(key: &str) -> Option<usize> {
    POINTS.iter().position(|p| p.key == key)
}

/// Reads a point's numeric value out of a parsed realtime message (booleans become 0/1).
///
/// Returns `None` when the field is absent or `null` (for example physics fields without a TLE).
///
/// # Examples
///
/// ```
/// use chronus_gateway::openmct::{POINTS, point_index, point_value};
///
/// let msg = serde_json::json!({"elevation_deg": 31.5, "rf_matched": true, "vcid": null});
/// let el = &POINTS[point_index("chronus.physics.elevation_deg").unwrap()];
/// assert_eq!(point_value(&msg, el), Some(31.5));
/// let rf = &POINTS[point_index("chronus.rf.matched").unwrap()];
/// assert_eq!(point_value(&msg, rf), Some(1.0));
/// let vc = &POINTS[point_index("chronus.tm.vcid").unwrap()];
/// assert_eq!(point_value(&msg, vc), None);
/// ```
#[must_use]
pub fn point_value(message: &Value, point: &PointDef) -> Option<f64> {
    match message.get(point.field)? {
        Value::Number(n) => n.as_f64().filter(|v| v.is_finite()),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        _ => None,
    }
}

/// The meaning of each `physics_flags` / `physics_flags_v2` bit, for dashboards.
const FLAG_BITS: &[(&str, u8, &str)] = &[
    (
        "physics_flags",
        0,
        "Doppler: measured carrier outside tolerance",
    ),
    (
        "physics_flags",
        1,
        "Below horizon: predicted elevation under the minimum",
    ),
    (
        "physics_flags",
        2,
        "Link budget: received power inconsistent with range",
    ),
    (
        "physics_flags",
        3,
        "Pointing: antenna not on the predicted look angle",
    ),
    (
        "physics_flags",
        4,
        "EPS (HIL lab model): bus voltage vs illumination",
    ),
    (
        "physics_flags",
        5,
        "Thermal (HIL lab model): temperature vs illumination",
    ),
    (
        "physics_flags",
        6,
        "Body rate (HIL lab model): attitude rate ceiling",
    ),
    (
        "physics_flags_v2",
        0,
        "Time-tag skew: onboard time disagrees with receive time",
    ),
    ("physics_flags_v2", 1, "Time code invalid"),
];

/// Builds the Open MCT dictionary for this station.
///
/// The name is taken from the TLE's title line when the ephemeris is an inline or file TLE with
/// one (for example `ISS (ZARYA)`), so the Open MCT tree is labelled with what is being tracked.
#[must_use]
pub fn dictionary(station: &StationConfig) -> Value {
    let spacecraft = spacecraft_name(station);
    let measurements: Vec<Value> = POINTS
        .iter()
        .map(|p| {
            let mut value = json!({
                "key": "value",
                "name": p.name,
                "format": p.format,
                "hints": {"range": 1},
            });
            if let Some(u) = p.units {
                value["units"] = json!(u);
            }
            if p.alarm_when_nonzero {
                value["chronus_alarm_when_nonzero"] = json!(true);
            }
            json!({
                "key": p.key,
                "name": p.name,
                "chronus_field": p.field,
                "values": [
                    value,
                    {
                        "key": "utc",
                        "source": "timestamp",
                        "name": "Ground receipt time",
                        "format": "utc",
                        "hints": {"domain": 1},
                    },
                ],
            })
        })
        .collect();
    let bits: Vec<Value> = FLAG_BITS
        .iter()
        .map(|(field, bit, meaning)| json!({"field": field, "bit": bit, "meaning": meaning}))
        .collect();
    json!({
        "chronus_schema": DICTIONARY_SCHEMA_V1,
        "name": spacecraft.unwrap_or_else(|| "ChronusGateway".to_string()),
        "key": "chronus",
        "realtime": {"websocket": "/telemetry/openmct", "schema": "openmct.realtime.v1"},
        "history": {"messages": "/api/v1/chronus/history", "point": "/api/v1/chronus/openmct/history/{key}"},
        "measurements": measurements,
        "flag_bits": bits,
    })
}

fn spacecraft_name(station: &StationConfig) -> Option<String> {
    let text = match &station.tracking.ephemeris {
        EphemerisSource::TleInline(t) => t.clone(),
        EphemerisSource::TleFile(_) => station.resolve_tle_text().ok()?,
        _ => return None,
    };
    let first = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    // A 3-line set starts with a title; a 2-line set starts with "1 ".
    (!first.starts_with("1 ")).then(|| first.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_fields_are_unique() {
        let mut keys: Vec<_> = POINTS.iter().map(|p| p.key).collect();
        let mut fields: Vec<_> = POINTS.iter().map(|p| p.field).collect();
        keys.sort_unstable();
        fields.sort_unstable();
        keys.dedup();
        fields.dedup();
        assert_eq!(keys.len(), POINTS.len());
        assert_eq!(fields.len(), POINTS.len());
    }

    #[test]
    fn dictionary_lists_every_point_with_range_and_domain_hints() {
        let d = dictionary(&StationConfig::default());
        assert_eq!(d["chronus_schema"], DICTIONARY_SCHEMA_V1);
        assert_eq!(d["name"], "ISS (ZARYA)");
        let m = d["measurements"].as_array().unwrap();
        assert_eq!(m.len(), POINTS.len());
        for (row, p) in m.iter().zip(POINTS) {
            assert_eq!(row["key"], p.key);
            assert_eq!(row["chronus_field"], p.field);
            assert_eq!(row["values"][0]["hints"]["range"], 1);
            assert_eq!(row["values"][1]["hints"]["domain"], 1);
            assert_eq!(row["values"][1]["source"], "timestamp");
        }
        assert_eq!(d["flag_bits"].as_array().unwrap().len(), 9);
    }

    #[test]
    fn two_line_tle_gives_the_generic_name() {
        let mut s = StationConfig::default();
        let two: String = crate::config::DEFAULT_ISS_TLE
            .lines()
            .skip(1)
            .collect::<Vec<_>>()
            .join("\n");
        s.tracking.ephemeris = EphemerisSource::TleInline(two);
        assert_eq!(dictionary(&s)["name"], "ChronusGateway");
    }

    #[test]
    fn point_value_ignores_strings_and_non_finite() {
        let p = &POINTS[point_index("chronus.tm.apid").unwrap()];
        assert_eq!(point_value(&json!({"apid": "x"}), p), None);
        assert_eq!(point_value(&json!({}), p), None);
        assert_eq!(point_value(&json!({"apid": 7}), p), Some(7.0));
        assert!(point_index("nope").is_none());
    }
}
