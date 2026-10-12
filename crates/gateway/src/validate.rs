//! Physics–Telemetry Co-Validation (Milestone 4).
//!
//! Cross-checks each [`TelemetryFrame`] against the propagator-derived [`TrackingState`] and sets
//! [`TelemetryFrame::physics_flags`] for downstream consumers (e.g. Open MCT alarm coloring).
//!
//! ## Doppler model
//!
//! Non-relativistic line-of-sight Doppler using the signed range rate from Ephemerust
//! (`range_rate_km_s`, **positive = receding**, **negative = approaching**):
//!
//! `f_expected = f_nominal − f_nominal × (v_m/s / c)` where `v_m/s = range_rate_km_s × 1000`.
//!
//! This matches the project design document convention (Δf from relative range rate). Since
//! Ephemerust 1.0 the range rate agrees with an independent IAU 2000 reference to 0.41 m/s
//! (≈ 0.6 Hz at 437.5 MHz) and with its own d(range)/dt to < 1 m/s (**T-RANGERATE**, enforced
//! here by `propagator::tests::range_rate_matches_numerical_derivative_of_range`), so the
//! propagation math is negligible inside the **±150 Hz** band. That band is consumed by TLE age
//! first (along-track error near closest approach, roughly 25 Hz per km on a high LEO pass at
//! UHF), then ionosphere, receiver chain and oscillator drift (see `TEST_PLAN.md` T-DOPPLER and
//! `Methodology.md` D-012, D-041).
//!
//! ## Link budget (free-space, CV-1)
//!
//! When [`RfMetadata::measured_rx_power_dbm`] is `Some`, compare to the **free-space** prediction
//! (Friis path loss in dB from slant range and carrier wavelength; no atmosphere or cable loss in
//! v1 — see **T-RSSI** in `TEST_PLAN.md`). Station-side **synthetic** `P_tx`, `G_tx`, and `G_rx`
//! come from [`LinkBudgetStationParams`]. Anomaly sets bit 2 ([`FLAG_LINK_BUDGET_ANOMALY`]).
//!
//! ## HIL subsystem toy checks (**CV-4** / **CV-5**) — `hil` feature
//!
//! Built only with the **`hil`** cargo feature (off by default; `Methodology.md` D-046). Without
//! it, bits 4–6 are never set.
//!
//! When a frame carries decoded [`crate::hil_tm::DecodedHilTmV1`] and `HilSubsystemCvParams` is
//! supplied, expected **bus voltage** and **panel temperature** are linear maps of
//! [`crate::propagator::TrackingState::nadir_sun_illum_cos`] (see `Methodology.md` **D-021**, **CV-4**).
//! **`body_rate_deg_s`** is checked against a configurable absolute ceiling (**CV-5**, **D-022**)
//! independent of Sun illumination. Checks are **skipped** when inputs are non-finite, decode fails,
//! or tunables are invalid — never panic on untrusted floats.
//!
//! ## `physics_flags` bitfield (stable contract)
//!
//! Shipped (M4) and planned extended co-validation bits are **chartered** in `Methodology.md`
//! **D-016** and [`docs/EXTENDED_COVALIDATION_PLAN.md`](../../../docs/EXTENDED_COVALIDATION_PLAN.md)
//! (CV-0). Do not repurpose bits without updating those documents and `TEST_PLAN.md`.
//!
//! | Bit | Mask | Meaning | Status |
//! |-----|------|---------|--------|
//! | 0 | `0x01` | Doppler anomaly — measured carrier differs from expected beyond tolerance. | **M4** |
//! | 1 | `0x02` | Horizon / elevation — spacecraft is below the configured minimum elevation. | **M4** |
//! | 2 | `0x04` | Link budget — measured vs predicted received power (**T-RSSI**, free-space v1). | **CV-1** |
//! | 3 | `0x08` | Pointing residual — measured vs computed (az, el) exceeds **T-POINT**. | **CV-2** (shipped) |
//! | 4 | `0x10` | EPS / bus voltage vs toy sun-angle model (**T-EPS**). | **CV-4** (shipped) |
//! | 5 | `0x20` | Thermal vs sun-angle proxy band (**T-THERMAL**). | **CV-4** (shipped) |
//! | 6 | `0x40` | ADCS — HIL `body_rate_deg_s` magnitude exceeds **T-BODYRATE** (**CV-5**). | **CV-5** (shipped) |
//! | 7 | `0x80` | Reserved. | — |
//!
//! If more than eight flags are needed, add `physics_flags_v2` (or similar) to the Open MCT JSON
//! envelope rather than silently reusing reserved bits (D-016).
//!
//! When [`RfMetadata::measured_carrier_hz`] is `None`, the Doppler check is **skipped** (no bit 0);
//! lab and integration tests pass `Some`. Production wiring from SDR metadata arrives with the
//! distribution layer or a dedicated ingest side-channel.
//!
//! When [`RfMetadata::measured_rx_power_dbm`] is `None`, the link-budget check is **skipped** (no bit 2).
//!
//! ## Pointing residual (CV-2)
//!
//! When **both** [`RfMetadata::measured_azimuth_deg`] and [`RfMetadata::measured_elevation_deg`] are
//! `Some`, compare the measured look direction to the propagator’s azimuth/elevation using the
//! great-circle angle (**T-POINT** in `TEST_PLAN.md`). Anomaly sets bit 3 ([`FLAG_POINTING_ANOMALY`]).
//! The computed elevation is the **apparent** one ([`TrackingState::pointing_elevation_deg`]) when
//! the station models refraction (default; D-042): an antenna tracking a signal points ~0.5° above
//! the geometric direction at the horizon, which would otherwise exceed the 0.25° tolerance. The
//! horizon check (bit 1) stays geometric.
//!
//! ## TLE-age Doppler margin (D-044)
//!
//! When the backend reports [`TrackingState::range_rate_uncertainty_km_s`] (opt-in
//! `doppler_tle_age_margin`), the Doppler band becomes
//! [`effective_doppler_tolerance_hz`]: **T-DOPPLER** plus `f₀ · δρ̇ / c`.

use std::f64::consts::PI;

use crate::ccsds::TelemetryFrame;
#[cfg(feature = "hil")]
use crate::hil_tm::{DecodedHilTmV1, decode_hil_tm_v1};
use crate::propagator::TrackingState;

/// Speed of light in vacuum (m/s); CODATA-compatible constant for Doppler arithmetic.
pub const SPEED_OF_LIGHT_M_S: f64 = 299_792_458.0;

/// Bit 0 — measured carrier frequency inconsistent with range-rate Doppler.
pub const FLAG_DOPPLER_ANOMALY: u8 = 0x01;
/// Bit 1 — spacecraft below the configured minimum elevation (e.g. below local horizon).
pub const FLAG_BELOW_HORIZON: u8 = 0x02;
/// Bit 2 — measured received power (dBm) inconsistent with **free-space** link budget (**CV-1**).
///
/// Historical name: [`FLAG_RSSI_RESERVED`] (same value; retained for API stability).
pub const FLAG_LINK_BUDGET_ANOMALY: u8 = 0x04;
/// Bit 2 — alias for [`FLAG_LINK_BUDGET_ANOMALY`] (legacy name).
pub const FLAG_RSSI_RESERVED: u8 = FLAG_LINK_BUDGET_ANOMALY;
/// Bit 3 — measured boresight (az/el) disagrees with computed look angles beyond **T-POINT** (**CV-2**).
pub const FLAG_POINTING_ANOMALY: u8 = 0x08;
/// Bit 4 — decoded HIL bus voltage inconsistent with toy sun-illumination model (**CV-4** / **T-EPS**).
pub const FLAG_EPS_SUBSYSTEM_ANOMALY: u8 = 0x10;
/// Bit 5 — decoded HIL panel temperature outside toy thermal band (**CV-4** / **T-THERMAL**).
pub const FLAG_THERMAL_SUBSYSTEM_ANOMALY: u8 = 0x20;
/// Bit 6 — decoded HIL \|`body_rate_deg_s`\| exceeds configured ceiling (**CV-5** / **T-BODYRATE**).
pub const FLAG_ADCS_BODY_RATE_ANOMALY: u8 = 0x40;

/// Optional **ground / receiver-chain** measurements accompanying a frame (SDR or synthetic lab).
///
/// Per **D-016**: carrier frequency for Doppler; optional **dBm** receive power for the link-budget
/// check (**CV-1**); optional **measured** azimuth/elevation (degrees) for the pointing check (**CV-2**).
/// Spacecraft-reported engineering scalars use the versioned CCSDS payload path (**CV-3** decode),
/// not this struct.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct RfMetadata {
    /// Measured downlink carrier frequency (Hz). `None` skips the Doppler check.
    pub measured_carrier_hz: Option<f64>,
    /// Measured received power (dBm), synthetic contract. `None` skips the link-budget check.
    pub measured_rx_power_dbm: Option<f64>,
    /// Measured antenna azimuth (degrees, clockwise from true north). **Both** this and
    /// [`Self::measured_elevation_deg`] must be `Some` to run the pointing check.
    pub measured_azimuth_deg: Option<f64>,
    /// Measured antenna elevation (degrees above local horizon).
    pub measured_elevation_deg: Option<f64>,
}

impl RfMetadata {
    /// No receiver measurements: every physics check that needs RF input is skipped.
    ///
    /// The struct is `#[non_exhaustive]` so measurement channels can be added in 1.x without
    /// breaking callers; build values with this constructor and the `with_*` methods.
    ///
    /// ```
    /// use chronus_gateway::validate::RfMetadata;
    ///
    /// let rf = RfMetadata::new()
    ///     .with_carrier_hz(437_500_000.0)
    ///     .with_rx_power_dbm(-110.0)
    ///     .with_pointing_deg(120.0, 35.0);
    /// assert_eq!(rf.measured_carrier_hz, Some(437_500_000.0));
    /// assert_eq!(rf.measured_elevation_deg, Some(35.0));
    /// assert!(RfMetadata::new().measured_rx_power_dbm.is_none());
    /// ```
    #[must_use]
    pub const fn new() -> Self {
        Self {
            measured_carrier_hz: None,
            measured_rx_power_dbm: None,
            measured_azimuth_deg: None,
            measured_elevation_deg: None,
        }
    }

    /// Sets the measured downlink carrier frequency (Hz) for the Doppler check.
    #[must_use]
    pub const fn with_carrier_hz(mut self, hz: f64) -> Self {
        self.measured_carrier_hz = Some(hz);
        self
    }

    /// Sets the measured received power (dBm) for the link-budget check.
    #[must_use]
    pub const fn with_rx_power_dbm(mut self, dbm: f64) -> Self {
        self.measured_rx_power_dbm = Some(dbm);
        self
    }

    /// Sets the measured antenna azimuth and elevation (degrees) for the pointing check.
    #[must_use]
    pub const fn with_pointing_deg(mut self, azimuth_deg: f64, elevation_deg: f64) -> Self {
        self.measured_azimuth_deg = Some(azimuth_deg);
        self.measured_elevation_deg = Some(elevation_deg);
        self
    }
}

/// Station-side **synthetic** parameters for the free-space link-budget prediction (CV-1).
#[derive(Debug, Clone, Copy)]
pub struct LinkBudgetStationParams {
    /// Transmit power at the feed (dBm).
    pub tx_power_dbm: f64,
    /// Transmit antenna gain (dBi).
    pub tx_gain_dbi: f64,
    /// Receive antenna gain (dBi).
    pub rx_gain_dbi: f64,
    /// Acceptable \|measured − predicted\| received power (dB); typically **T-RSSI** (3 dB).
    pub tolerance_db: f64,
}

#[cfg(feature = "hil")]
/// Tunables for **CV-4** / **CV-5** HIL subsystem cross-checks (synthetic demo only).
///
/// Built from [`crate::config::StationConfig`] via [`ValidationContext::from_station`].
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct HilSubsystemCvParams {
    /// Inclusive lower bound of CCSDS APIDs carrying `chronus.hil.tm.v1` payloads (**CV-3**).
    pub hil_tm_v1_apid_min: u16,
    /// Inclusive upper bound of CCSDS APIDs carrying `chronus.hil.tm.v1` payloads.
    pub hil_tm_v1_apid_max: u16,
    /// Model bus voltage at full toy illumination (`nadir_sun_illum_cos` = 1), volts.
    pub hil_eps_voltage_full_sun_v: f64,
    /// Model bus voltage at zero illumination, volts.
    pub hil_eps_voltage_eclipse_v: f64,
    /// Panel temperature at full illumination, °C.
    pub hil_thermal_c_full_sun: f64,
    /// Panel temperature at zero illumination, °C.
    pub hil_thermal_c_eclipse: f64,
    /// **T-EPS:** allowed relative deviation vs the voltage span (e.g. `0.1` = ±10 % of span).
    pub hil_eps_relative_tolerance: f64,
    /// **T-THERMAL:** allowed ± deviation from the linear thermal model, kelvin (°C numerically).
    pub hil_thermal_absolute_tolerance_k: f64,
    /// **T-BODYRATE:** maximum allowed \|`body_rate_deg_s`\| from decoded HIL v1 (deg/s).
    pub hil_body_rate_max_abs_deg_s: f64,
}

#[cfg(feature = "hil")]
impl HilSubsystemCvParams {
    /// Copies CV-4 / CV-5 fields from a validated [`crate::config::StationConfig`].
    #[must_use]
    pub fn from_station(station: &crate::config::StationConfig) -> Self {
        Self {
            hil_tm_v1_apid_min: station.hil.tm_v1_apid_min,
            hil_tm_v1_apid_max: station.hil.tm_v1_apid_max,
            hil_eps_voltage_full_sun_v: station.hil.eps_voltage_full_sun_v,
            hil_eps_voltage_eclipse_v: station.hil.eps_voltage_eclipse_v,
            hil_thermal_c_full_sun: station.hil.thermal_c_full_sun,
            hil_thermal_c_eclipse: station.hil.thermal_c_eclipse,
            hil_eps_relative_tolerance: station.hil.eps_relative_tolerance,
            hil_thermal_absolute_tolerance_k: station.hil.thermal_absolute_tolerance_k,
            hil_body_rate_max_abs_deg_s: station.hil.body_rate_max_abs_deg_s,
        }
    }

    /// `true` when `apid` lies in the configured synthetic HIL TM v1 range (inclusive).
    #[must_use]
    pub fn allows_apid(&self, apid: u16) -> bool {
        (self.hil_tm_v1_apid_min..=self.hil_tm_v1_apid_max).contains(&apid)
    }
}

/// Station-level inputs to [`apply_physics_validation`], built **once** (at startup or per
/// session) instead of being threaded through every call as positional arguments.
///
/// Per-frame inputs (the frame, its [`TrackingState`] and [`RfMetadata`]) stay separate, so a new
/// check adds a field here and a branch in the validator without changing any signature. The
/// struct is `#[non_exhaustive]`: build it with [`ValidationContext::from_station`] or
/// [`ValidationContext::new`] plus the `with_*` methods.
///
/// ```
/// use chronus_gateway::config::StationConfig;
/// use chronus_gateway::validate::ValidationContext;
///
/// let ctx = ValidationContext::from_station(&StationConfig::default());
/// assert_eq!(ctx.doppler_tolerance_hz, 150.0);
/// assert!(ctx.link_budget.is_some());
///
/// // Hand-built: Doppler + horizon only.
/// let ctx = ValidationContext::new(437_500_000.0).with_doppler_tolerance_hz(200.0);
/// assert!(ctx.link_budget.is_none());
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ValidationContext {
    /// Nominal (un-shifted) downlink carrier (Hz) the Doppler and link-budget checks start from.
    pub nominal_carrier_hz: f64,
    /// **T-DOPPLER:** allowed \|measured − expected\| carrier deviation (Hz) before any TLE-age
    /// margin (see [`effective_doppler_tolerance_hz`]).
    pub doppler_tolerance_hz: f64,
    /// Minimum elevation (degrees) below which bit 1 ([`FLAG_BELOW_HORIZON`]) is set.
    pub minimum_elevation_deg: f64,
    /// **T-POINT:** great-circle pointing tolerance (degrees); non-positive disables the check.
    pub pointing_tolerance_deg: f64,
    /// Link-budget parameters (**CV-1**); `None` skips the check.
    pub link_budget: Option<LinkBudgetStationParams>,
    /// HIL subsystem toy-check parameters (**CV-4** / **CV-5**); `None` skips them. Present only
    /// with the `hil` cargo feature.
    #[cfg(feature = "hil")]
    pub hil: Option<HilSubsystemCvParams>,
}

impl ValidationContext {
    /// Minimal context: Doppler (±150 Hz) and horizon (0°) only; no link budget, pointing
    /// (tolerance 0) or HIL checks until enabled with the `with_*` methods.
    #[must_use]
    pub fn new(nominal_carrier_hz: f64) -> Self {
        Self {
            nominal_carrier_hz,
            doppler_tolerance_hz: 150.0,
            minimum_elevation_deg: 0.0,
            pointing_tolerance_deg: 0.0,
            link_budget: None,
            #[cfg(feature = "hil")]
            hil: None,
        }
    }

    /// Builds the full context from a validated [`crate::config::StationConfig`].
    #[must_use]
    pub fn from_station(station: &crate::config::StationConfig) -> Self {
        Self {
            nominal_carrier_hz: station.site.nominal_carrier_hz,
            doppler_tolerance_hz: station.tolerances.doppler_tolerance_hz,
            minimum_elevation_deg: station.tolerances.minimum_elevation_deg,
            pointing_tolerance_deg: station.tolerances.pointing_tolerance_deg,
            link_budget: Some(LinkBudgetStationParams {
                tx_power_dbm: station.tolerances.tx_power_dbm,
                tx_gain_dbi: station.tolerances.tx_gain_dbi,
                rx_gain_dbi: station.tolerances.rx_gain_dbi,
                tolerance_db: station.tolerances.link_budget_tolerance_db,
            }),
            #[cfg(feature = "hil")]
            hil: Some(HilSubsystemCvParams::from_station(station)),
        }
    }

    /// Sets **T-DOPPLER** (Hz).
    #[must_use]
    pub fn with_doppler_tolerance_hz(mut self, hz: f64) -> Self {
        self.doppler_tolerance_hz = hz;
        self
    }

    /// Sets the minimum elevation (degrees) for the horizon check.
    #[must_use]
    pub fn with_minimum_elevation_deg(mut self, deg: f64) -> Self {
        self.minimum_elevation_deg = deg;
        self
    }

    /// Enables the pointing check with **T-POINT** (degrees).
    #[must_use]
    pub fn with_pointing_tolerance_deg(mut self, deg: f64) -> Self {
        self.pointing_tolerance_deg = deg;
        self
    }

    /// Enables the link-budget check.
    #[must_use]
    pub fn with_link_budget(mut self, lb: LinkBudgetStationParams) -> Self {
        self.link_budget = Some(lb);
        self
    }

    /// Enables the HIL subsystem toy checks (requires the `hil` cargo feature).
    #[cfg(feature = "hil")]
    #[must_use]
    pub fn with_hil(mut self, hil: HilSubsystemCvParams) -> Self {
        self.hil = Some(hil);
        self
    }
}

/// Expected received carrier (Hz) from nominal transmit frequency and line-of-sight range rate.
///
/// `range_rate_km_s` follows Ephemerust: positive receding, negative approaching.
///
/// # Examples
///
/// ```
/// use chronus_gateway::validate::expected_carrier_hz;
///
/// let f0 = 437_500_000.0;
/// // Approaching at 7 km/s: received frequency is shifted up by roughly 10 kHz.
/// let shift = expected_carrier_hz(f0, -7.0) - f0;
/// assert!((shift - 10_215.0).abs() < 5.0, "shift = {shift}");
/// // No relative motion: no shift.
/// assert_eq!(expected_carrier_hz(f0, 0.0), f0);
/// ```
#[must_use]
pub fn expected_carrier_hz(nominal_hz: f64, range_rate_km_s: f64) -> f64 {
    let v_m_s = range_rate_km_s * 1000.0;
    nominal_hz - nominal_hz * (v_m_s / SPEED_OF_LIGHT_M_S)
}

/// Free-space path loss \(L_{fs}\) in dB: `20 log10(4π R / λ)` with `R` in metres, `λ = c / f`.
///
/// Returns `None` if inputs are non-finite, non-positive, or the ratio is invalid.
///
/// # Examples
///
/// ```
/// use chronus_gateway::validate::free_space_path_loss_db;
///
/// // 1000 km at 437.5 MHz is about 145 dB of loss.
/// let l = free_space_path_loss_db(1.0e6, 437.5e6).unwrap();
/// assert!((l - 145.27).abs() < 0.05, "L = {l}");
/// // Doubling distance adds ~6 dB.
/// let l2 = free_space_path_loss_db(2.0e6, 437.5e6).unwrap();
/// assert!((l2 - l - 6.02).abs() < 0.01);
/// // Nonsense input returns None instead of NaN.
/// assert!(free_space_path_loss_db(-1.0, 437.5e6).is_none());
/// ```
#[must_use]
pub fn free_space_path_loss_db(range_m: f64, carrier_hz: f64) -> Option<f64> {
    if !(range_m.is_finite() && carrier_hz.is_finite()) || range_m <= 0.0 || carrier_hz <= 0.0 {
        return None;
    }
    let lambda = SPEED_OF_LIGHT_M_S / carrier_hz;
    if !(lambda.is_finite() && lambda > 0.0) {
        return None;
    }
    let ratio = (4.0 * PI * range_m) / lambda;
    if !(ratio.is_finite() && ratio > 0.0) {
        return None;
    }
    Some(20.0 * ratio.log10())
}

/// Predicted received power (dBm) in free space: `P_tx + G_tx + G_rx − L_fs`.
///
/// # Examples
///
/// ```
/// use chronus_gateway::validate::expected_rx_power_dbm;
///
/// // 30 dBm transmitter, 2 dBi + 5 dBi antennas, 1000 km away at 437.5 MHz.
/// let p = expected_rx_power_dbm(1000.0, 437.5e6, 30.0, 2.0, 5.0).unwrap();
/// assert!((p - (-108.27)).abs() < 0.05, "P = {p}");
/// ```
#[must_use]
pub fn expected_rx_power_dbm(
    range_km: f64,
    carrier_hz: f64,
    tx_power_dbm: f64,
    tx_gain_dbi: f64,
    rx_gain_dbi: f64,
) -> Option<f64> {
    let range_m = range_km * 1000.0;
    let l_fs = free_space_path_loss_db(range_m, carrier_hz)?;
    Some(tx_power_dbm + tx_gain_dbi + rx_gain_dbi - l_fs)
}

/// Great-circle separation between two look directions on the unit sphere (degrees).
///
/// Each direction uses **azimuth** clockwise from true north (degrees) and **elevation** above the
/// local horizon (degrees), matching [`TrackingState`].
///
/// Returns `None` if any input is non-finite.
///
/// # Examples
///
/// ```
/// use chronus_gateway::validate::angular_separation_deg;
///
/// // Due north on the horizon vs due east on the horizon: a quarter turn apart.
/// let sep = angular_separation_deg(0.0, 0.0, 90.0, 0.0).unwrap();
/// assert!((sep - 90.0).abs() < 1e-9);
/// // At the zenith, azimuth doesn't matter.
/// let sep = angular_separation_deg(0.0, 90.0, 180.0, 90.0).unwrap();
/// assert!(sep < 1e-6);
/// assert!(angular_separation_deg(f64::NAN, 0.0, 0.0, 0.0).is_none());
/// ```
#[must_use]
pub fn angular_separation_deg(
    az1_deg: f64,
    el1_deg: f64,
    az2_deg: f64,
    el2_deg: f64,
) -> Option<f64> {
    if !(az1_deg.is_finite() && el1_deg.is_finite() && az2_deg.is_finite() && el2_deg.is_finite()) {
        return None;
    }
    let u1 = az_el_to_enu_unit(az1_deg, el1_deg)?;
    let u2 = az_el_to_enu_unit(az2_deg, el2_deg)?;
    let dot = u1.0 * u2.0 + u1.1 * u2.1 + u1.2 * u2.2;
    let clamped = dot.clamp(-1.0_f64, 1.0_f64);
    let rad = clamped.acos();
    Some(rad.to_degrees())
}

/// Local east–north–up unit vector from azimuth (° from north, clockwise) and elevation (°).
fn az_el_to_enu_unit(az_deg: f64, el_deg: f64) -> Option<(f64, f64, f64)> {
    let az = az_deg.to_radians();
    let el = el_deg.to_radians();
    let ce = el.cos();
    if !ce.is_finite() {
        return None;
    }
    let e = ce * az.sin();
    let n = ce * az.cos();
    let u = el.sin();
    if !(e.is_finite() && n.is_finite() && u.is_finite()) {
        return None;
    }
    Some((e, n, u))
}

/// Doppler acceptance band (Hz) for one frame: the configured **T-DOPPLER** tolerance plus the
/// frequency equivalent of the ephemeris range-rate uncertainty
/// ([`TrackingState::range_rate_uncertainty_km_s`], D-044; zero unless the TLE-age margin is on).
///
/// ```
/// use chronus_gateway::propagator::TrackingState;
/// use chronus_gateway::validate::effective_doppler_tolerance_hz;
///
/// let fixed = TrackingState::new(0.0, 45.0, 800.0, -2.0);
/// assert_eq!(effective_doppler_tolerance_hz(150.0, 437.5e6, &fixed), 150.0);
///
/// // A 40 m/s range-rate uncertainty at 437.5 MHz is ~58 Hz of extra margin.
/// let aged = fixed.with_range_rate_uncertainty(0.040);
/// let tol = effective_doppler_tolerance_hz(150.0, 437.5e6, &aged);
/// assert!((tol - 208.4).abs() < 0.1, "{tol}");
/// ```
#[must_use]
pub fn effective_doppler_tolerance_hz(
    doppler_tolerance_hz: f64,
    nominal_carrier_hz: f64,
    state: &TrackingState,
) -> f64 {
    let u = state.range_rate_uncertainty_km_s;
    if u.is_finite() && u > 0.0 && nominal_carrier_hz.is_finite() {
        doppler_tolerance_hz + nominal_carrier_hz * (u * 1000.0) / SPEED_OF_LIGHT_M_S
    } else {
        doppler_tolerance_hz
    }
}

/// Clears then sets [`TelemetryFrame::physics_flags`] from physics checks.
///
/// Always resets flags to `0` first so repeated validation does not OR stale bits.
/// Non-finite `TrackingState` fields skip checks that depend on them (no panic).
///
/// Station-level tunables ([`ValidationContext`]) are built once; only the per-frame inputs
/// (`frame`, `state`, `rf`) change per call, so adding a check never changes this signature.
/// A context with `link_budget: None` skips the link-budget check entirely, and a non-finite or
/// non-positive `pointing_tolerance_deg` (**T-POINT**) skips the pointing check.
pub fn apply_physics_validation(
    ctx: &ValidationContext,
    frame: &mut TelemetryFrame,
    state: &TrackingState,
    rf: RfMetadata,
) {
    let nominal_carrier_hz = ctx.nominal_carrier_hz;
    let doppler_tolerance_hz = ctx.doppler_tolerance_hz;
    let minimum_elevation_deg = ctx.minimum_elevation_deg;
    let link_budget = ctx.link_budget;
    let pointing_tolerance_deg = ctx.pointing_tolerance_deg;

    frame.physics_flags = 0;

    if state.elevation_deg.is_finite() && state.elevation_deg < minimum_elevation_deg {
        frame.physics_flags |= FLAG_BELOW_HORIZON;
    }

    if let Some(measured) = rf.measured_carrier_hz
        && measured.is_finite()
        && nominal_carrier_hz.is_finite()
        && state.range_rate_km_s.is_finite()
    {
        let expected = expected_carrier_hz(nominal_carrier_hz, state.range_rate_km_s);
        let tolerance =
            effective_doppler_tolerance_hz(doppler_tolerance_hz, nominal_carrier_hz, state);
        if (measured - expected).abs() > tolerance {
            frame.physics_flags |= FLAG_DOPPLER_ANOMALY;
        }
    }

    if let Some(meas_dbm) = rf.measured_rx_power_dbm
        && meas_dbm.is_finite()
        && let Some(lb) = link_budget
        && lb.tolerance_db.is_finite()
        && lb.tolerance_db > 0.0
        && lb.tx_power_dbm.is_finite()
        && lb.tx_gain_dbi.is_finite()
        && lb.rx_gain_dbi.is_finite()
        && state.range_km.is_finite()
        && let Some(pred) = expected_rx_power_dbm(
            state.range_km,
            nominal_carrier_hz,
            lb.tx_power_dbm,
            lb.tx_gain_dbi,
            lb.rx_gain_dbi,
        )
        && pred.is_finite()
        && (meas_dbm - pred).abs() > lb.tolerance_db
    {
        frame.physics_flags |= FLAG_LINK_BUDGET_ANOMALY;
    }

    if pointing_tolerance_deg.is_finite()
        && pointing_tolerance_deg > 0.0
        && let (Some(maz), Some(mel)) = (rf.measured_azimuth_deg, rf.measured_elevation_deg)
        && maz.is_finite()
        && mel.is_finite()
        && state.azimuth_deg.is_finite()
        && state.pointing_elevation_deg().is_finite()
        && let Some(sep) =
            angular_separation_deg(maz, mel, state.azimuth_deg, state.pointing_elevation_deg())
        && sep.is_finite()
        && sep > pointing_tolerance_deg
    {
        frame.physics_flags |= FLAG_POINTING_ANOMALY;
    }

    // CV-4 / CV-5 — decoded HIL v1 (only for APIDs in the configured synthetic range).
    #[cfg(feature = "hil")]
    {
        if let Some(p) = ctx.hil.as_ref()
            && p.allows_apid(frame.apid)
            && let Ok(hil) = decode_hil_tm_v1(frame.payload())
        {
            apply_hil_subsystem_checks(frame, state, hil, p);
        }
    }
}

#[cfg(feature = "hil")]
/// Runs the **CV-4** / **CV-5** HIL subsystem toy checks for one decoded `chronus.hil.tm.v1`
/// payload, OR-ing any anomaly bits into [`TelemetryFrame::physics_flags`] (it does not reset them).
///
/// Skipped per check when inputs or tunables are non-finite; never panics on untrusted floats.
fn apply_hil_subsystem_checks(
    frame: &mut TelemetryFrame,
    state: &TrackingState,
    hil: DecodedHilTmV1,
    p: &HilSubsystemCvParams,
) {
    // CV-5 — ADCS body-rate envelope (independent of Sun illumination).
    if p.hil_body_rate_max_abs_deg_s.is_finite() && p.hil_body_rate_max_abs_deg_s > 0.0 {
        let w = hil.body_rate_deg_s as f64;
        if w.is_finite() && w.abs() > p.hil_body_rate_max_abs_deg_s {
            frame.physics_flags |= FLAG_ADCS_BODY_RATE_ANOMALY;
        }
    }

    let illum = state.nadir_sun_illum_cos;
    if illum.is_finite() && (0.0..=1.0).contains(&illum) {
        if p.hil_eps_relative_tolerance.is_finite()
            && p.hil_eps_relative_tolerance > 0.0
            && p.hil_eps_voltage_full_sun_v.is_finite()
            && p.hil_eps_voltage_eclipse_v.is_finite()
        {
            let v_exp = p.hil_eps_voltage_eclipse_v
                + (p.hil_eps_voltage_full_sun_v - p.hil_eps_voltage_eclipse_v) * illum;
            if v_exp.is_finite() {
                let span = (p.hil_eps_voltage_full_sun_v - p.hil_eps_voltage_eclipse_v).abs();
                let tol_v = (p.hil_eps_relative_tolerance * span.max(1e-6)).max(1e-6);
                let v_meas = hil.eps_bus_voltage_v as f64;
                if v_meas.is_finite() && (v_meas - v_exp).abs() > tol_v {
                    frame.physics_flags |= FLAG_EPS_SUBSYSTEM_ANOMALY;
                }
            }
        }

        if p.hil_thermal_absolute_tolerance_k.is_finite()
            && p.hil_thermal_absolute_tolerance_k > 0.0
            && p.hil_thermal_c_full_sun.is_finite()
            && p.hil_thermal_c_eclipse.is_finite()
        {
            let t_exp = p.hil_thermal_c_eclipse
                + (p.hil_thermal_c_full_sun - p.hil_thermal_c_eclipse) * illum;
            if t_exp.is_finite() {
                let tol_t = p.hil_thermal_absolute_tolerance_k;
                let t_meas = hil.thermal_panel_c as f64;
                if t_meas.is_finite() && (t_meas - t_exp).abs() > tol_t {
                    frame.physics_flags |= FLAG_THERMAL_SUBSYSTEM_ANOMALY;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::propagator::TrackingState;
    use chrono::Utc;
    use std::net::SocketAddr;

    /// Default **T-POINT** for tests (`TEST_PLAN.md`).
    const T_POINT: f64 = 0.25;

    #[cfg(feature = "hil")]
    type HilTmArg = DecodedHilTmV1;
    #[cfg(feature = "hil")]
    type HilParamsArg = HilSubsystemCvParams;
    #[cfg(not(feature = "hil"))]
    type HilTmArg = ();
    #[cfg(not(feature = "hil"))]
    type HilParamsArg = ();

    /// Test shim: builds a one-off [`ValidationContext`] from explicit station tunables, runs the
    /// validator, and (for tests that hand-feed a decoded payload) the HIL subsystem checks.
    #[allow(clippy::too_many_arguments)]
    fn run_validation(
        frame: &mut TelemetryFrame,
        state: &TrackingState,
        nominal_carrier_hz: f64,
        rf: RfMetadata,
        doppler_tolerance_hz: f64,
        minimum_elevation_deg: f64,
        link_budget: Option<LinkBudgetStationParams>,
        pointing_tolerance_deg: f64,
        hil_tm: Option<HilTmArg>,
        hil_subsystem: Option<HilParamsArg>,
    ) {
        let mut ctx = ValidationContext::new(nominal_carrier_hz)
            .with_doppler_tolerance_hz(doppler_tolerance_hz)
            .with_minimum_elevation_deg(minimum_elevation_deg)
            .with_pointing_tolerance_deg(pointing_tolerance_deg);
        ctx.link_budget = link_budget;
        apply_physics_validation(&ctx, frame, state, rf);
        // The decoded payload is supplied directly, so bypass the APID-range decode path.
        #[cfg(feature = "hil")]
        if let (Some(hil), Some(p)) = (hil_tm, hil_subsystem) {
            apply_hil_subsystem_checks(frame, state, hil, &p);
        }
        #[cfg(not(feature = "hil"))]
        let _ = (hil_tm, hil_subsystem);
    }

    fn dummy_tm() -> TelemetryFrame {
        TelemetryFrame {
            raw: vec![0u8; 12].into(),
            payload_len: 6,
            apid: 1,
            seq_count: 0,
            has_secondary_header: false,
            received_at: Utc::now(),
            source: SocketAddr::from(([127, 0, 0, 1], 1)),
            physics_flags: 0,
        }
    }

    fn state(el_deg: f64, range_rate_km_s: f64) -> TrackingState {
        TrackingState {
            azimuth_deg: 90.0,
            elevation_deg: el_deg,
            range_km: 4000.0,
            range_rate_km_s,
            nadir_sun_illum_cos: f64::NAN,
            apparent_elevation_deg: f64::NAN,
            range_rate_uncertainty_km_s: 0.0,
        }
    }

    #[test]
    fn expected_carrier_matches_non_relativistic_formula() {
        let f0 = 1e9;
        let v_m_s = 3000.0;
        let v_km_s = v_m_s / 1000.0;
        let exp = expected_carrier_hz(f0, v_km_s);
        let manual = f0 - f0 * (v_m_s / SPEED_OF_LIGHT_M_S);
        assert!((exp - manual).abs() < 1e-6, "exp={exp} manual={manual}");
    }

    #[test]
    fn doppler_in_band_no_flag_t_doppler_150hz() {
        // T-DOPPLER: ±150 Hz — measured within band of expected.
        let mut tm = dummy_tm();
        let s = state(45.0, 0.2648964327533433); // ISS-like range rate from propagator tests
        let f0 = 437_500_000.0;
        let expected = expected_carrier_hz(f0, s.range_rate_km_s);
        run_validation(
            &mut tm,
            &s,
            f0,
            RfMetadata {
                measured_carrier_hz: Some(expected + 100.0), // +100 Hz < 150
                measured_rx_power_dbm: None,
                ..Default::default()
            },
            150.0,
            0.0,
            None,
            T_POINT,
            None,
            None,
        );
        assert_eq!(tm.physics_flags & FLAG_DOPPLER_ANOMALY, 0);
        assert_eq!(tm.physics_flags & FLAG_BELOW_HORIZON, 0);
    }

    #[test]
    fn doppler_out_of_band_sets_bit0() {
        let mut tm = dummy_tm();
        let s = state(45.0, 0.0);
        let f0 = 437_500_000.0;
        let expected = expected_carrier_hz(f0, 0.0);
        run_validation(
            &mut tm,
            &s,
            f0,
            RfMetadata {
                measured_carrier_hz: Some(expected + 200.0), // > 150 Hz
                measured_rx_power_dbm: None,
                ..Default::default()
            },
            150.0,
            0.0,
            None,
            T_POINT,
            None,
            None,
        );
        assert!(tm.physics_flags & FLAG_DOPPLER_ANOMALY != 0);
    }

    #[test]
    fn below_horizon_sets_bit1() {
        let mut tm = dummy_tm();
        let s = state(-5.0, 0.0);
        run_validation(
            &mut tm,
            &s,
            437_500_000.0,
            RfMetadata::default(),
            150.0,
            0.0,
            None,
            T_POINT,
            None,
            None,
        );
        assert!(tm.physics_flags & FLAG_BELOW_HORIZON != 0);
        assert_eq!(tm.physics_flags & FLAG_DOPPLER_ANOMALY, 0);
    }

    #[test]
    fn elevation_at_horizon_not_flagged_when_minimum_is_zero() {
        let mut tm = dummy_tm();
        let s = state(0.0, 0.0);
        run_validation(
            &mut tm,
            &s,
            437e6,
            RfMetadata::default(),
            150.0,
            0.0,
            None,
            T_POINT,
            None,
            None,
        );
        assert_eq!(tm.physics_flags & FLAG_BELOW_HORIZON, 0);
    }

    #[test]
    fn combined_anomalies_set_both_bits() {
        let mut tm = dummy_tm();
        let s = state(-10.0, 0.0);
        let f0 = 100e6;
        run_validation(
            &mut tm,
            &s,
            f0,
            RfMetadata {
                measured_carrier_hz: Some(f0 + 500.0),
                measured_rx_power_dbm: None,
                ..Default::default()
            },
            150.0,
            0.0,
            None,
            T_POINT,
            None,
            None,
        );
        assert_eq!(tm.physics_flags, FLAG_DOPPLER_ANOMALY | FLAG_BELOW_HORIZON);
    }

    #[test]
    fn no_measured_carrier_skips_doppler_even_if_would_be_bad() {
        let mut tm = dummy_tm();
        let s = state(30.0, 0.0);
        run_validation(
            &mut tm,
            &s,
            100e6,
            RfMetadata::default(),
            150.0,
            0.0,
            None,
            T_POINT,
            None,
            None,
        );
        assert_eq!(tm.physics_flags, 0);
    }

    #[test]
    fn independent_bits_doppler_only() {
        let mut tm = dummy_tm();
        let s = state(30.0, 0.0);
        let f0 = 100e6;
        run_validation(
            &mut tm,
            &s,
            f0,
            RfMetadata {
                measured_carrier_hz: Some(f0 + 500.0),
                measured_rx_power_dbm: None,
                ..Default::default()
            },
            150.0,
            0.0,
            None,
            T_POINT,
            None,
            None,
        );
        assert_eq!(tm.physics_flags, FLAG_DOPPLER_ANOMALY);
    }

    #[test]
    fn nan_measured_skips_doppler_no_panic() {
        let mut tm = dummy_tm();
        let s = state(10.0, 1.0);
        run_validation(
            &mut tm,
            &s,
            437e6,
            RfMetadata {
                measured_carrier_hz: Some(f64::NAN),
                measured_rx_power_dbm: None,
                ..Default::default()
            },
            150.0,
            0.0,
            None,
            T_POINT,
            None,
            None,
        );
        assert_eq!(tm.physics_flags, 0);
    }

    fn demo_lb() -> LinkBudgetStationParams {
        LinkBudgetStationParams {
            tx_power_dbm: 30.0,
            tx_gain_dbi: 2.0,
            rx_gain_dbi: 5.0,
            tolerance_db: 3.0,
        }
    }

    #[test]
    fn free_space_path_loss_matches_manual() {
        let range_m = 4_000_000.0;
        let f = 437_500_000.0;
        let l = free_space_path_loss_db(range_m, f).expect("L_fs");
        let lambda = SPEED_OF_LIGHT_M_S / f;
        let manual = 20.0 * ((4.0 * PI * range_m) / lambda).log10();
        assert!((l - manual).abs() < 1e-9, "l={l} manual={manual}");
    }

    #[test]
    fn link_budget_in_band_no_flag_t_rssi() {
        let mut tm = dummy_tm();
        let s = state(45.0, 0.0);
        let f0 = 437_500_000.0;
        let pred = expected_rx_power_dbm(s.range_km, f0, 30.0, 2.0, 5.0).expect("pred");
        run_validation(
            &mut tm,
            &s,
            f0,
            RfMetadata {
                measured_carrier_hz: None,
                measured_rx_power_dbm: Some(pred + 2.0), // within ±3 dB
                ..Default::default()
            },
            150.0,
            0.0,
            Some(demo_lb()),
            T_POINT,
            None,
            None,
        );
        assert_eq!(tm.physics_flags & FLAG_LINK_BUDGET_ANOMALY, 0);
    }

    #[test]
    fn link_budget_out_of_band_sets_bit2() {
        let mut tm = dummy_tm();
        let s = state(45.0, 0.0);
        let f0 = 437_500_000.0;
        let pred = expected_rx_power_dbm(s.range_km, f0, 30.0, 2.0, 5.0).expect("pred");
        run_validation(
            &mut tm,
            &s,
            f0,
            RfMetadata {
                measured_carrier_hz: None,
                measured_rx_power_dbm: Some(pred + 5.0), // > 3 dB
                ..Default::default()
            },
            150.0,
            0.0,
            Some(demo_lb()),
            T_POINT,
            None,
            None,
        );
        assert!(tm.physics_flags & FLAG_LINK_BUDGET_ANOMALY != 0);
        assert_eq!(tm.physics_flags & FLAG_DOPPLER_ANOMALY, 0);
    }

    #[test]
    fn no_measured_rx_skips_link_budget_even_if_would_be_bad() {
        let mut tm = dummy_tm();
        let s = state(45.0, 0.0);
        run_validation(
            &mut tm,
            &s,
            437_500_000.0,
            RfMetadata::default(),
            150.0,
            0.0,
            Some(demo_lb()),
            T_POINT,
            None,
            None,
        );
        assert_eq!(tm.physics_flags, 0);
    }

    #[test]
    fn nan_measured_rx_skips_link_no_panic() {
        let mut tm = dummy_tm();
        let s = state(45.0, 0.0);
        run_validation(
            &mut tm,
            &s,
            437_500_000.0,
            RfMetadata {
                measured_carrier_hz: None,
                measured_rx_power_dbm: Some(f64::NAN),
                ..Default::default()
            },
            150.0,
            0.0,
            Some(demo_lb()),
            T_POINT,
            None,
            None,
        );
        assert_eq!(tm.physics_flags & FLAG_LINK_BUDGET_ANOMALY, 0);
    }

    #[test]
    fn zero_range_skips_link_budget_no_flag() {
        let mut tm = dummy_tm();
        let s = TrackingState {
            azimuth_deg: 0.0,
            elevation_deg: 45.0,
            range_km: 0.0,
            range_rate_km_s: 0.0,
            nadir_sun_illum_cos: f64::NAN,
            apparent_elevation_deg: f64::NAN,
            range_rate_uncertainty_km_s: 0.0,
        };
        run_validation(
            &mut tm,
            &s,
            437_500_000.0,
            RfMetadata {
                measured_carrier_hz: None,
                measured_rx_power_dbm: Some(-120.0),
                ..Default::default()
            },
            150.0,
            0.0,
            Some(demo_lb()),
            T_POINT,
            None,
            None,
        );
        assert_eq!(tm.physics_flags & FLAG_LINK_BUDGET_ANOMALY, 0);
    }

    #[test]
    fn angular_separation_same_direction_near_zero() {
        let sep = angular_separation_deg(90.0, 45.0, 90.0, 45.0).expect("sep");
        assert!(sep < 1e-9, "sep={sep}");
    }

    #[test]
    fn angular_separation_orthogonal_ninety_deg() {
        let sep = angular_separation_deg(0.0, 0.0, 90.0, 0.0).expect("sep");
        assert!((sep - 90.0).abs() < 1e-6, "sep={sep}");
    }

    fn flags_for(s: &TrackingState, rf: RfMetadata) -> u8 {
        let mut tm = dummy_tm();
        run_validation(
            &mut tm, s, 437.5e6, rf, 150.0, 0.0, None, T_POINT, None, None,
        );
        tm.physics_flags
    }

    /// D-042: near the horizon an antenna tracks the refracted (apparent) direction, ~0.4° above
    /// geometric at 1° elevation — more than T-POINT. With refraction modelled, a correctly
    /// pointed antenna passes and one pointed at the geometric direction is flagged.
    #[test]
    fn pointing_uses_apparent_elevation_when_refraction_is_modelled() {
        let geometric = 1.0;
        let apparent = geometric
            + ephemerust::coordinates::refraction_deg_with_conditions(geometric, 1010.0, 10.0);
        assert!(
            apparent - geometric > T_POINT,
            "refraction at 1° must exceed T-POINT"
        );
        let s = TrackingState::new(200.0, geometric, 2300.0, 0.0).with_apparent_elevation(apparent);
        let rf = |el: f64| RfMetadata {
            measured_azimuth_deg: Some(200.0),
            measured_elevation_deg: Some(el),
            ..Default::default()
        };
        assert_eq!(flags_for(&s, rf(apparent)) & FLAG_POINTING_ANOMALY, 0);
        assert_ne!(flags_for(&s, rf(geometric)) & FLAG_POINTING_ANOMALY, 0);
        // Without refraction modelled the geometric direction is the reference.
        let plain = TrackingState::new(200.0, geometric, 2300.0, 0.0);
        assert_eq!(flags_for(&plain, rf(geometric)) & FLAG_POINTING_ANOMALY, 0);
        // The horizon check stays geometric: apparent > 0 does not hide a geometric -0.3°.
        let low = TrackingState::new(200.0, -0.3, 2500.0, 0.0).with_apparent_elevation(0.2);
        assert_ne!(
            flags_for(&low, RfMetadata::default()) & FLAG_BELOW_HORIZON,
            0
        );
    }

    /// D-044: the TLE-age range-rate uncertainty widens the Doppler band by f0·δρ̇/c.
    #[test]
    fn tle_age_uncertainty_widens_the_doppler_band() {
        let f0 = 437.5e6;
        let base = TrackingState::new(90.0, 45.0, 800.0, -1.5);
        let measured = expected_carrier_hz(f0, base.range_rate_km_s) + 200.0; // 200 Hz off
        let rf = RfMetadata {
            measured_carrier_hz: Some(measured),
            ..Default::default()
        };
        assert_ne!(
            flags_for(&base, rf) & FLAG_DOPPLER_ANOMALY,
            0,
            "fixed 150 Hz band"
        );
        // 40 m/s of range-rate uncertainty ≈ 58 Hz → band ≈ 208 Hz: accepted.
        let aged = base.with_range_rate_uncertainty(0.040);
        assert_eq!(
            flags_for(&aged, rf) & FLAG_DOPPLER_ANOMALY,
            0,
            "widened band"
        );
        // A spoof 2 kHz off is still caught.
        let spoof = RfMetadata {
            measured_carrier_hz: Some(measured + 1800.0),
            ..Default::default()
        };
        assert_ne!(flags_for(&aged, spoof) & FLAG_DOPPLER_ANOMALY, 0);
        // Garbage uncertainty never narrows or disables the check.
        for u in [f64::NAN, -1.0, f64::INFINITY] {
            let t = effective_doppler_tolerance_hz(150.0, f0, &base.with_range_rate_uncertainty(u));
            assert_eq!(t, 150.0, "u = {u}");
        }
    }

    #[test]
    fn pointing_within_t_point_no_bit3() {
        let mut tm = dummy_tm();
        let s = TrackingState {
            azimuth_deg: 90.0,
            elevation_deg: 45.0,
            range_km: 4000.0,
            range_rate_km_s: 0.0,
            nadir_sun_illum_cos: f64::NAN,
            apparent_elevation_deg: f64::NAN,
            range_rate_uncertainty_km_s: 0.0,
        };
        run_validation(
            &mut tm,
            &s,
            437e6,
            RfMetadata {
                measured_carrier_hz: None,
                measured_rx_power_dbm: None,
                measured_azimuth_deg: Some(90.1),
                measured_elevation_deg: Some(45.0),
            },
            150.0,
            0.0,
            None,
            T_POINT,
            None,
            None,
        );
        assert_eq!(tm.physics_flags & FLAG_POINTING_ANOMALY, 0);
    }

    #[test]
    fn pointing_exceeds_t_point_sets_bit3() {
        let mut tm = dummy_tm();
        let s = TrackingState {
            azimuth_deg: 90.0,
            elevation_deg: 45.0,
            range_km: 4000.0,
            range_rate_km_s: 0.0,
            nadir_sun_illum_cos: f64::NAN,
            apparent_elevation_deg: f64::NAN,
            range_rate_uncertainty_km_s: 0.0,
        };
        run_validation(
            &mut tm,
            &s,
            437e6,
            RfMetadata {
                measured_carrier_hz: None,
                measured_rx_power_dbm: None,
                measured_azimuth_deg: Some(92.0),
                measured_elevation_deg: Some(45.0),
            },
            150.0,
            0.0,
            None,
            T_POINT,
            None,
            None,
        );
        assert!(tm.physics_flags & FLAG_POINTING_ANOMALY != 0);
    }

    #[test]
    fn pointing_only_azimuth_skips_no_bit3() {
        let mut tm = dummy_tm();
        let s = TrackingState {
            azimuth_deg: 90.0,
            elevation_deg: 45.0,
            range_km: 4000.0,
            range_rate_km_s: 0.0,
            nadir_sun_illum_cos: f64::NAN,
            apparent_elevation_deg: f64::NAN,
            range_rate_uncertainty_km_s: 0.0,
        };
        run_validation(
            &mut tm,
            &s,
            437e6,
            RfMetadata {
                measured_carrier_hz: None,
                measured_rx_power_dbm: None,
                measured_azimuth_deg: Some(200.0),
                measured_elevation_deg: None,
            },
            150.0,
            0.0,
            None,
            T_POINT,
            None,
            None,
        );
        assert_eq!(tm.physics_flags & FLAG_POINTING_ANOMALY, 0);
    }

    #[test]
    fn non_finite_pointing_tolerance_skips_pointing() {
        let mut tm = dummy_tm();
        let s = TrackingState {
            azimuth_deg: 90.0,
            elevation_deg: 45.0,
            range_km: 4000.0,
            range_rate_km_s: 0.0,
            nadir_sun_illum_cos: f64::NAN,
            apparent_elevation_deg: f64::NAN,
            range_rate_uncertainty_km_s: 0.0,
        };
        run_validation(
            &mut tm,
            &s,
            437e6,
            RfMetadata {
                measured_carrier_hz: None,
                measured_rx_power_dbm: None,
                measured_azimuth_deg: Some(200.0),
                measured_elevation_deg: Some(45.0),
            },
            150.0,
            0.0,
            None,
            0.0,
            None,
            None,
        );
        assert_eq!(tm.physics_flags & FLAG_POINTING_ANOMALY, 0);
    }

    #[cfg(feature = "hil")]
    #[test]
    fn hil_cv4_voltage_and_thermal_in_band() {
        let mut tm = dummy_tm();
        let illum = 0.8125_f64;
        let p = HilSubsystemCvParams {
            hil_tm_v1_apid_min: crate::hil_tm::DEFAULT_HIL_TM_V1_APID_MIN,
            hil_tm_v1_apid_max: crate::hil_tm::DEFAULT_HIL_TM_V1_APID_MAX,
            hil_eps_voltage_full_sun_v: 28.0,
            hil_eps_voltage_eclipse_v: 24.0,
            hil_thermal_c_full_sun: 32.0,
            hil_thermal_c_eclipse: 12.0,
            hil_eps_relative_tolerance: 0.11,
            hil_thermal_absolute_tolerance_k: 10.0,
            hil_body_rate_max_abs_deg_s: 5.0,
        };
        let v_exp = 24.0 + (28.0 - 24.0) * illum;
        let t_exp = 12.0 + (32.0 - 12.0) * illum;
        let hil = crate::hil_tm::DecodedHilTmV1 {
            seq: 1,
            eps_bus_voltage_v: v_exp as f32,
            thermal_panel_c: t_exp as f32,
            body_rate_deg_s: 0.0,
        };
        let s = TrackingState {
            azimuth_deg: 0.0,
            elevation_deg: 45.0,
            range_km: 4000.0,
            range_rate_km_s: 0.0,
            nadir_sun_illum_cos: illum,
            apparent_elevation_deg: f64::NAN,
            range_rate_uncertainty_km_s: 0.0,
        };
        run_validation(
            &mut tm,
            &s,
            437e6,
            RfMetadata::default(),
            150.0,
            0.0,
            None,
            T_POINT,
            Some(hil),
            Some(p),
        );
        assert_eq!(
            tm.physics_flags & (FLAG_EPS_SUBSYSTEM_ANOMALY | FLAG_THERMAL_SUBSYSTEM_ANOMALY),
            0
        );
    }

    #[cfg(feature = "hil")]
    #[test]
    fn hil_cv4_bad_voltage_sets_bit4() {
        let mut tm = dummy_tm();
        let illum = 0.5_f64;
        let p = HilSubsystemCvParams {
            hil_tm_v1_apid_min: crate::hil_tm::DEFAULT_HIL_TM_V1_APID_MIN,
            hil_tm_v1_apid_max: crate::hil_tm::DEFAULT_HIL_TM_V1_APID_MAX,
            hil_eps_voltage_full_sun_v: 28.0,
            hil_eps_voltage_eclipse_v: 24.0,
            hil_thermal_c_full_sun: 32.0,
            hil_thermal_c_eclipse: 12.0,
            hil_eps_relative_tolerance: 0.1,
            hil_thermal_absolute_tolerance_k: 10.0,
            hil_body_rate_max_abs_deg_s: 5.0,
        };
        let v_ok = 24.0 + (28.0 - 24.0) * illum;
        let t_ok = 12.0 + (32.0 - 12.0) * illum;
        let hil = crate::hil_tm::DecodedHilTmV1 {
            seq: 0,
            eps_bus_voltage_v: (v_ok + 1.0) as f32,
            thermal_panel_c: t_ok as f32,
            body_rate_deg_s: 0.0,
        };
        let s = TrackingState {
            azimuth_deg: 0.0,
            elevation_deg: 45.0,
            range_km: 4000.0,
            range_rate_km_s: 0.0,
            nadir_sun_illum_cos: illum,
            apparent_elevation_deg: f64::NAN,
            range_rate_uncertainty_km_s: 0.0,
        };
        run_validation(
            &mut tm,
            &s,
            437e6,
            RfMetadata::default(),
            150.0,
            0.0,
            None,
            T_POINT,
            Some(hil),
            Some(p),
        );
        assert!(tm.physics_flags & FLAG_EPS_SUBSYSTEM_ANOMALY != 0);
        assert_eq!(tm.physics_flags & FLAG_THERMAL_SUBSYSTEM_ANOMALY, 0);
    }

    #[cfg(feature = "hil")]
    #[test]
    fn hil_cv4_bad_thermal_sets_bit5() {
        let mut tm = dummy_tm();
        let illum = 0.25_f64;
        let p = HilSubsystemCvParams {
            hil_tm_v1_apid_min: crate::hil_tm::DEFAULT_HIL_TM_V1_APID_MIN,
            hil_tm_v1_apid_max: crate::hil_tm::DEFAULT_HIL_TM_V1_APID_MAX,
            hil_eps_voltage_full_sun_v: 28.0,
            hil_eps_voltage_eclipse_v: 24.0,
            hil_thermal_c_full_sun: 32.0,
            hil_thermal_c_eclipse: 12.0,
            hil_eps_relative_tolerance: 0.1,
            hil_thermal_absolute_tolerance_k: 10.0,
            hil_body_rate_max_abs_deg_s: 5.0,
        };
        let v_ok = 24.0 + (28.0 - 24.0) * illum;
        let t_ok = 12.0 + (32.0 - 12.0) * illum;
        let hil = crate::hil_tm::DecodedHilTmV1 {
            seq: 0,
            eps_bus_voltage_v: v_ok as f32,
            thermal_panel_c: (t_ok + 15.0) as f32,
            body_rate_deg_s: 0.0,
        };
        let s = TrackingState {
            azimuth_deg: 0.0,
            elevation_deg: 45.0,
            range_km: 4000.0,
            range_rate_km_s: 0.0,
            nadir_sun_illum_cos: illum,
            apparent_elevation_deg: f64::NAN,
            range_rate_uncertainty_km_s: 0.0,
        };
        run_validation(
            &mut tm,
            &s,
            437e6,
            RfMetadata::default(),
            150.0,
            0.0,
            None,
            T_POINT,
            Some(hil),
            Some(p),
        );
        assert_eq!(tm.physics_flags & FLAG_EPS_SUBSYSTEM_ANOMALY, 0);
        assert!(tm.physics_flags & FLAG_THERMAL_SUBSYSTEM_ANOMALY != 0);
    }

    #[cfg(feature = "hil")]
    #[test]
    fn hil_cv4_skips_when_illum_non_finite() {
        let mut tm = dummy_tm();
        let p = HilSubsystemCvParams {
            hil_tm_v1_apid_min: crate::hil_tm::DEFAULT_HIL_TM_V1_APID_MIN,
            hil_tm_v1_apid_max: crate::hil_tm::DEFAULT_HIL_TM_V1_APID_MAX,
            hil_eps_voltage_full_sun_v: 28.0,
            hil_eps_voltage_eclipse_v: 24.0,
            hil_thermal_c_full_sun: 32.0,
            hil_thermal_c_eclipse: 12.0,
            hil_eps_relative_tolerance: 0.1,
            hil_thermal_absolute_tolerance_k: 10.0,
            hil_body_rate_max_abs_deg_s: 5.0,
        };
        let hil = crate::hil_tm::DecodedHilTmV1 {
            seq: 0,
            eps_bus_voltage_v: 999.0,
            thermal_panel_c: 999.0,
            body_rate_deg_s: 0.0,
        };
        let s = TrackingState {
            azimuth_deg: 0.0,
            elevation_deg: 45.0,
            range_km: 4000.0,
            range_rate_km_s: 0.0,
            nadir_sun_illum_cos: f64::NAN,
            apparent_elevation_deg: f64::NAN,
            range_rate_uncertainty_km_s: 0.0,
        };
        run_validation(
            &mut tm,
            &s,
            437e6,
            RfMetadata::default(),
            150.0,
            0.0,
            None,
            T_POINT,
            Some(hil),
            Some(p),
        );
        assert_eq!(
            tm.physics_flags & (FLAG_EPS_SUBSYSTEM_ANOMALY | FLAG_THERMAL_SUBSYSTEM_ANOMALY),
            0
        );
    }

    #[cfg(feature = "hil")]
    #[test]
    fn hil_cv5_body_rate_within_ceiling_no_bit6() {
        let mut tm = dummy_tm();
        let p = HilSubsystemCvParams {
            hil_tm_v1_apid_min: crate::hil_tm::DEFAULT_HIL_TM_V1_APID_MIN,
            hil_tm_v1_apid_max: crate::hil_tm::DEFAULT_HIL_TM_V1_APID_MAX,
            hil_eps_voltage_full_sun_v: 28.0,
            hil_eps_voltage_eclipse_v: 24.0,
            hil_thermal_c_full_sun: 32.0,
            hil_thermal_c_eclipse: 12.0,
            hil_eps_relative_tolerance: 0.1,
            hil_thermal_absolute_tolerance_k: 10.0,
            hil_body_rate_max_abs_deg_s: 2.0,
        };
        let hil = crate::hil_tm::DecodedHilTmV1 {
            seq: 0,
            eps_bus_voltage_v: 0.0,
            thermal_panel_c: 0.0,
            body_rate_deg_s: 1.5,
        };
        let s = TrackingState {
            azimuth_deg: 0.0,
            elevation_deg: 45.0,
            range_km: 4000.0,
            range_rate_km_s: 0.0,
            nadir_sun_illum_cos: f64::NAN,
            apparent_elevation_deg: f64::NAN,
            range_rate_uncertainty_km_s: 0.0,
        };
        run_validation(
            &mut tm,
            &s,
            437e6,
            RfMetadata::default(),
            150.0,
            0.0,
            None,
            T_POINT,
            Some(hil),
            Some(p),
        );
        assert_eq!(tm.physics_flags & FLAG_ADCS_BODY_RATE_ANOMALY, 0);
    }

    #[cfg(feature = "hil")]
    #[test]
    fn hil_cv5_body_rate_exceeds_ceiling_sets_bit6() {
        let mut tm = dummy_tm();
        let p = HilSubsystemCvParams {
            hil_tm_v1_apid_min: crate::hil_tm::DEFAULT_HIL_TM_V1_APID_MIN,
            hil_tm_v1_apid_max: crate::hil_tm::DEFAULT_HIL_TM_V1_APID_MAX,
            hil_eps_voltage_full_sun_v: 28.0,
            hil_eps_voltage_eclipse_v: 24.0,
            hil_thermal_c_full_sun: 32.0,
            hil_thermal_c_eclipse: 12.0,
            hil_eps_relative_tolerance: 0.1,
            hil_thermal_absolute_tolerance_k: 10.0,
            hil_body_rate_max_abs_deg_s: 1.0,
        };
        let hil = crate::hil_tm::DecodedHilTmV1 {
            seq: 0,
            eps_bus_voltage_v: 0.0,
            thermal_panel_c: 0.0,
            body_rate_deg_s: -3.0,
        };
        let s = TrackingState {
            azimuth_deg: 0.0,
            elevation_deg: 45.0,
            range_km: 4000.0,
            range_rate_km_s: 0.0,
            nadir_sun_illum_cos: f64::NAN,
            apparent_elevation_deg: f64::NAN,
            range_rate_uncertainty_km_s: 0.0,
        };
        run_validation(
            &mut tm,
            &s,
            437e6,
            RfMetadata::default(),
            150.0,
            0.0,
            None,
            T_POINT,
            Some(hil),
            Some(p),
        );
        assert!(tm.physics_flags & FLAG_ADCS_BODY_RATE_ANOMALY != 0);
    }

    #[cfg(feature = "hil")]
    #[test]
    fn hil_cv5_skips_when_body_rate_non_finite() {
        let mut tm = dummy_tm();
        let p = HilSubsystemCvParams {
            hil_tm_v1_apid_min: crate::hil_tm::DEFAULT_HIL_TM_V1_APID_MIN,
            hil_tm_v1_apid_max: crate::hil_tm::DEFAULT_HIL_TM_V1_APID_MAX,
            hil_eps_voltage_full_sun_v: 28.0,
            hil_eps_voltage_eclipse_v: 24.0,
            hil_thermal_c_full_sun: 32.0,
            hil_thermal_c_eclipse: 12.0,
            hil_eps_relative_tolerance: 0.1,
            hil_thermal_absolute_tolerance_k: 10.0,
            hil_body_rate_max_abs_deg_s: 1.0,
        };
        let hil = crate::hil_tm::DecodedHilTmV1 {
            seq: 0,
            eps_bus_voltage_v: 0.0,
            thermal_panel_c: 0.0,
            body_rate_deg_s: f32::NAN,
        };
        let s = TrackingState {
            azimuth_deg: 0.0,
            elevation_deg: 45.0,
            range_km: 4000.0,
            range_rate_km_s: 0.0,
            nadir_sun_illum_cos: f64::NAN,
            apparent_elevation_deg: f64::NAN,
            range_rate_uncertainty_km_s: 0.0,
        };
        run_validation(
            &mut tm,
            &s,
            437e6,
            RfMetadata::default(),
            150.0,
            0.0,
            None,
            T_POINT,
            Some(hil),
            Some(p),
        );
        assert_eq!(tm.physics_flags & FLAG_ADCS_BODY_RATE_ANOMALY, 0);
    }

    #[cfg(feature = "hil")]
    #[test]
    fn hil_cv5_skips_when_max_not_positive() {
        let mut tm = dummy_tm();
        let p = HilSubsystemCvParams {
            hil_tm_v1_apid_min: crate::hil_tm::DEFAULT_HIL_TM_V1_APID_MIN,
            hil_tm_v1_apid_max: crate::hil_tm::DEFAULT_HIL_TM_V1_APID_MAX,
            hil_eps_voltage_full_sun_v: 28.0,
            hil_eps_voltage_eclipse_v: 24.0,
            hil_thermal_c_full_sun: 32.0,
            hil_thermal_c_eclipse: 12.0,
            hil_eps_relative_tolerance: 0.1,
            hil_thermal_absolute_tolerance_k: 10.0,
            hil_body_rate_max_abs_deg_s: f64::NAN,
        };
        let hil = crate::hil_tm::DecodedHilTmV1 {
            seq: 0,
            eps_bus_voltage_v: 0.0,
            thermal_panel_c: 0.0,
            body_rate_deg_s: 100.0,
        };
        let s = TrackingState {
            azimuth_deg: 0.0,
            elevation_deg: 45.0,
            range_km: 4000.0,
            range_rate_km_s: 0.0,
            nadir_sun_illum_cos: f64::NAN,
            apparent_elevation_deg: f64::NAN,
            range_rate_uncertainty_km_s: 0.0,
        };
        run_validation(
            &mut tm,
            &s,
            437e6,
            RfMetadata::default(),
            150.0,
            0.0,
            None,
            T_POINT,
            Some(hil),
            Some(p),
        );
        assert_eq!(tm.physics_flags & FLAG_ADCS_BODY_RATE_ANOMALY, 0);
    }

    #[cfg(feature = "hil")]
    fn parsed_hil_frame(apid: u16, body_rate: f32) -> TelemetryFrame {
        use crate::hil_tm::encode_hil_tm_v1_payload;
        use crate::ingest::RawFrame;
        use std::sync::Arc;
        let payload = encode_hil_tm_v1_payload(1, 26.0, 22.0, body_rate);
        let packet = crate::ccsds::encode_synthetic_tm(apid, 0, &payload);
        crate::ccsds::parse_telemetry(&RawFrame {
            bytes: Arc::from(packet.as_slice()),
            received_at: Utc::now(),
            source: SocketAddr::from(([127, 0, 0, 1], 1)),
        })
        .expect("synthetic packet parses")
    }

    #[cfg(feature = "hil")]
    #[test]
    fn context_from_station_mirrors_config() {
        let station = crate::config::StationConfig::default();
        let ctx = ValidationContext::from_station(&station);
        assert_eq!(ctx.nominal_carrier_hz, station.site.nominal_carrier_hz);
        assert_eq!(
            ctx.doppler_tolerance_hz,
            station.tolerances.doppler_tolerance_hz
        );
        assert_eq!(
            ctx.pointing_tolerance_deg,
            station.tolerances.pointing_tolerance_deg
        );
        let lb = ctx.link_budget.expect("link budget enabled from station");
        assert_eq!(lb.tolerance_db, station.tolerances.link_budget_tolerance_db);
        #[cfg(feature = "hil")]
        assert!(ctx.hil.is_some());
    }

    #[cfg(feature = "hil")]
    #[test]
    fn context_decodes_hil_payload_only_inside_configured_apid_range() {
        let station = crate::config::StationConfig::default();
        let ctx = ValidationContext::from_station(&station);
        let s = state(45.0, 0.0);
        let rf = RfMetadata::new();

        // In range: a 90 deg/s body rate trips the CV-5 ceiling through the context path.
        let mut in_range = parsed_hil_frame(station.hil.tm_v1_apid_min, 90.0);
        apply_physics_validation(&ctx, &mut in_range, &s, rf);
        assert_ne!(in_range.physics_flags & FLAG_ADCS_BODY_RATE_ANOMALY, 0);

        // Same payload on an APID outside the range is opaque user data: no HIL checks.
        let outside = station.hil.tm_v1_apid_max + 1;
        let mut out_of_range = parsed_hil_frame(outside, 90.0);
        apply_physics_validation(&ctx, &mut out_of_range, &s, rf);
        assert_eq!(out_of_range.physics_flags & FLAG_ADCS_BODY_RATE_ANOMALY, 0);
    }

    #[test]
    fn minimal_context_runs_doppler_and_horizon_only() {
        let ctx = ValidationContext::new(437_500_000.0);
        let mut tm = dummy_tm();
        let s = state(-5.0, 0.0);
        let rf = RfMetadata::new()
            .with_rx_power_dbm(-200.0)
            .with_pointing_deg(0.0, 80.0);
        apply_physics_validation(&ctx, &mut tm, &s, rf);
        // Below horizon is flagged; link budget and pointing were never enabled.
        assert_eq!(tm.physics_flags, FLAG_BELOW_HORIZON);
    }

    #[cfg(not(feature = "hil"))]
    #[test]
    fn hil_subsystem_checks_are_inert_without_the_hil_feature() {
        use crate::hil_tm::encode_hil_tm_v1_payload;
        use crate::ingest::RawFrame;
        use std::sync::Arc;
        let station = crate::config::StationConfig::default();
        // A 90 deg/s body rate on an in-range APID would trip CV-5 in a `hil` build.
        let payload = encode_hil_tm_v1_payload(1, 26.0, 22.0, 90.0);
        let packet = crate::ccsds::encode_synthetic_tm(station.hil.tm_v1_apid_min, 0, &payload);
        let mut tm = crate::ccsds::parse_telemetry(&RawFrame {
            bytes: Arc::from(packet.as_slice()),
            received_at: Utc::now(),
            source: SocketAddr::from(([127, 0, 0, 1], 1)),
        })
        .unwrap();
        let ctx = ValidationContext::from_station(&station);
        let s = TrackingState::new(90.0, 45.0, 800.0, 0.0).with_sun_illumination(1.0);
        apply_physics_validation(&ctx, &mut tm, &s, RfMetadata::new());
        assert_eq!(
            tm.physics_flags
                & (FLAG_EPS_SUBSYSTEM_ANOMALY
                    | FLAG_THERMAL_SUBSYSTEM_ANOMALY
                    | FLAG_ADCS_BODY_RATE_ANOMALY),
            0
        );
    }
}
