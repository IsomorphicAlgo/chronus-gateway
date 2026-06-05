# Extended Physics–Telemetry Co-Validation — Iterative, Approval-Gated Plan

Companion to [`BUILD_PLAN.md`](BUILD_PLAN.md) (Milestones M0–M8 **complete**). This document defines **follow-on** work to implement the broader validation ideas from the design paper: link budget / RSSI, antenna pointing residual, and synthetic subsystem checks (EPS / thermal vs sun geometry).

**Governance (same as the main roadmap):**

> A milestone is **complete** only when its deliverables exist, its **test gate** is green (`TEST_PLAN.md`), and the **stage-gate checklist** is confirmed. **Do not chain milestones** — obtain **owner approval** before starting the next tranche.

Legend: `[x]` done · `[ ]` pending · **Gate** = owner sign-off required to advance.

**Compliance:** All examples, gains, powers, and HIL scenarios remain **synthetic and generic** per [`AGENTS.md`](../AGENTS.md). No mission-specific or export-controlled parameters.

---

## CV-0 — Scope, contracts, and bitfield charter **Gate only (no code)** — **charter drafted**

**Objective:** Lock the **public contract** so later milestones do not thrash Open MCT consumers or JSON fields.

**Deliverables**

- [x] **Bitfield map** — Recorded in **`Methodology.md` D-016**, module docs in `crates/gateway/src/validate.rs`, and the frozen table below. Bits 6–7 remain reserved; overflow policy: add **`physics_flags_v2`** (or similar) to JSON, do not repurpose bits silently.
- [x] **RfMetadata policy** — Ground-chain measurements on **`RfMetadata`**; spacecraft-reported scalars via **versioned synthetic CCSDS payload** after **CV-3**. Decision **D-016**.
- [x] **Tolerance register** — **`TEST_PLAN.md`** rows **T-RSSI**, **T-POINT**, **T-EPS**, **T-THERMAL** chartered with rationales (rebaseline when CV-1 / CV-2 / CV-4 models land).
- [x] **Explicit deferrals** — Listed in **D-016** (atmosphere, multipath, full PUS, SPICE-grade ephemeris, uncalibrated hardware RSSI, mission-specific TM mapping).

**Test gate:** N/A (documentation + charter only).

**Gate CV-0:** `[x]` **Owner approval** of this charter — **CV-1** implementation may proceed. *(Charter delivered 2026-06-03.)*

### Frozen charter — `physics_flags` (u8)

| Bit | Mask | Semantics | First milestone |
|-----|------|-----------|-----------------|
| 0 | `0x01` | Doppler anomaly | M4 (shipped) |
| 1 | `0x02` | Below minimum elevation | M4 (shipped) |
| 2 | `0x04` | Link budget / received power vs free-space prediction | CV-1 |
| 3 | `0x08` | Pointing residual (measured vs computed az/el, **T-POINT**) | CV-2 |
| 4 | `0x10` | EPS / array current vs toy sun model (**T-EPS**) | CV-4 |
| 5 | `0x20` | Thermal vs sun-angle proxy (**T-THERMAL**) | CV-4 |
| 6–7 | `0x40`–`0x80` | Reserved | — |

**CV-3** does not consume a dedicated flag bit; it delivers the **payload decode** contract that **CV-4** depends on.

---

## CV-1 — Link budget / RSSI co-validation (free-space, ±3 dB)

**Objective:** Compute expected received power from station + range + nominal frequency; compare to optional measured power; set **bit 2** on anomaly.

**Deliverables**

- [ ] **Physics** — Free-space path loss from `TrackingState::range_km` and wavelength from `nominal_carrier_hz`; combine with configurable `P_tx`, `G_tx`, `G_rx` (dBm / dBi, synthetic defaults).
- [ ] **`RfMetadata`** — Optional measured receive power (dBm) with clear naming (e.g. `measured_rx_power_dbm`).
- [ ] **`StationConfig` / TOML** — New optional section or fields; merge + validate + `gateway.example.toml` + file unit tests.
- [ ] **`apply_physics_validation`** — Skip when inputs missing or non-finite; never panic on untrusted floats.
- [ ] **Rename / document bit 2** — Treat `FLAG_RSSI_RESERVED` as the live **link-budget anomaly** flag (keep const name for API stability **or** add alias + deprecation note in one release).
- [ ] **Tests** — Golden numeric cases + edge (zero/negative range skip); update `TEST_PLAN.md` M4 / new subsection **CV-1**.
- [ ] **`Methodology.md`** — Rationale for free-space-only v1.

**Test gate:** `cargo test` green; new unit tests for link-budget pass/fail; `cargo clippy --all-targets` clean.

**Gate CV-1:** `[ ]` Owner approves behavior + tolerances — **only then** start CV-2.

---

## CV-2 — Antenna encoder vs computed boresight (angular error vs T-POINT, typically 0.25°)

**Objective:** Compare measured point direction to propagator-derived azimuth/elevation; flag when great-circle angular separation exceeds **T-POINT** (design target: 0.25° per project paper; exact value set at CV-0).

**Deliverables**

- [ ] **`RfMetadata`** — Optional `measured_azimuth_deg`, `measured_elevation_deg` (synthetic servo / tracking receiver).
- [ ] **Angular separation** — Spherical geometry helper (unit-tested independently of propagator).
- [ ] **`physics_flags`** — New bit (e.g. bit 3) per CV-0 charter.
- [ ] **Tests** — Below threshold, above threshold, horizon edge cases, non-finite skip; `TEST_PLAN.md` **CV-2**.
- [ ] **Docs** — `validate` module table + README one-liner.

**Test gate:** `cargo test` green; clippy clean.

**Gate CV-2:** `[ ]` Owner approves — **only then** start CV-3.

---

## CV-3 — Synthetic TM payload contract + decoder (no new physics yet)

**Objective:** Define a **versioned, bounded** binary layout in the CCSDS packet data field for HIL (and tests) so subsystem co-validation does not depend on ambiguous raw floats.

**Deliverables**

- [ ] **Schema `chronus.hil.tm.v1`** (or similar) — Magic/version byte(s), fixed widths, big-endian scalars, maximum size enforced against `payload_len`.
- [ ] **Decoder module** — Returns `Option<DecodedHilV1>` or structured error; **no allocation** beyond small stack struct when possible.
- [ ] **APID policy** — Either fixed synthetic APID band (documented) or configurable allowlist in `StationConfig`.
- [ ] **Tests** — Truncated payload, wrong version, wrong magic — safe rejection; golden round-trip.
- [ ] **HIL crate** — `chronus-hil-sim` emits the new layout (backward-compatible transition period optional: feature flag or separate APID).
- [ ] **`TEST_PLAN.md` + `docs/HIL.md`** — Update recipes.

**Test gate:** Gateway + HIL tests green; decoder covered by unit + integration smoke.

**Gate CV-3:** `[ ]` Owner approves payload layout frozen for sim — **only then** start CV-4.

---

## CV-4 — Subsystem co-validation (EPS current vs sun angle, thermal proxy)

**Objective:** Cross-check decoded telemetry against **geometry derived from time + orbit (+ optional attitude from TM)** using toy models suitable for open-source demo (not flight thermal/EPS fidelity).

**Deliverables**

- [ ] **Propagator / state seam** — Expose minimum extra state (e.g. ECI position or sun–satellite angle) needed for sun geometry, without breaking `OrbitalPropagator` consumers — design approved in CV-0/Methodology (extension to `TrackingState` vs new trait method).
- [ ] **Sun geometry** — Documented approximate algorithm (literature-cited); deterministic tests with fixed time/TLE.
- [ ] **Toy models** — e.g. expected array current ∝ `I_max * max(0, cos(theta))` with eclipse clamp; thermal bound vs sun angle band (document tolerances **T-EPS**, **T-THERMAL**).
- [ ] **`apply_physics_validation` (or submodule)** — New bits per charter; skip when attitude or required fields absent.
- [ ] **NeXosim alignment** — Sim produces self-consistent “good” passes; optional “fault injection” for tests only.
- [ ] **Tests** — Physics co-validation style: tolerances justified in `TEST_PLAN.md`.

**Test gate:** Full `cargo test`; extended register in `TEST_PLAN.md`; no new warnings.

**Gate CV-4:** `[ ]` Owner approves milestone complete; decide whether to fold summary into `BUILD_PLAN.md` as “M9 portfolio” or keep this doc as the canonical extension roadmap.

---

## Dependency graph

```text
CV-0 (charter) ──▶ CV-1 (link budget) ──▶ CV-2 (pointing)
                              │
                              ▼
                     CV-3 (payload schema + decode)
                              │
                              ▼
                     CV-4 (subsystem vs sun geometry)
```

**Critical path:** CV-0 → CV-1 → CV-2 can proceed with **only** `RfMetadata` + `TrackingState` today. CV-3 is the **enabler** for CV-4; attempting CV-4 before CV-3 invites undefined payload semantics.

---

## Integration notes (all CV milestones)

- **Wiring:** Today the entrypoint uses `RfMetadata::default()` for distribution paths; each CV milestone should state how tests and (optionally) CLI/env supply synthetic metadata without blocking UDP ingest security.
- **JSON / Open MCT:** Optional additive fields (`expected_rx_dbm`, …) only if needed; **bitfield remains the primary alarm surface** unless CV-0 chooses otherwise.
- **Benchmarks:** Extend `parse_validate` bench if CV-3+ adds measurable hot-path work.

---

## Backlog (explicitly not in CV-1–CV-4)

- Atmospheric / rain fade models, multipath, polarization.
- Full ECSS PUS / CUC secondary-header parsing for production spacecraft.
- Nyx-backed high-fidelity ephemeris (remains separate `BUILD_PLAN.md` / propagator backlog in this folder).
- CelesTrak / Space-Track auto TLE fetch (existing config backlog).

---

*Document version: 2026-06-03 (CV-0 charter drafted). Maintainer: update checkboxes and gates as milestones land.*
