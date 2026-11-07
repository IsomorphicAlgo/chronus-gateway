# IERS Earth orientation data (real, public)

`finals2000A.daily.csv`: the IERS Rapid Service / Prediction Centre's daily Earth orientation
parameters (Bulletin A, IAU 2000), in the IERS data centre's semicolon-separated CSV form. It was
downloaded by the repository owner on **2026-11-07** and holds 181 daily rows:

- **2026-08-10 to 2026-11-07:** observed values (`final`);
- **2026-11-08 to 2027-02-06:** predictions.

It is used two ways:

- **Test fixture.** `crates/gateway/src/freshness.rs` parses it, checks hand-copied values from
  its rows, and checks that it agrees with the same rows re-written in the fixed-width
  `finals2000A` layout (Methodology D-052).
- **Demo input.** Point `freshness.eop_file` at it, then run `chronus-gateway --check-config` to
  see the UT1−UTC and polar motion the gateway would use. On 2026-11-07 at 21:17 UTC the gateway
  printed UT1−UTC −0.0631669 s and pole 0.128846″ / 0.320593″. Interpolating the file's 11-07 and
  11-08 rows by hand gives the same values.

For live use, download a current copy rather than relying on this one: it ends on 2027-02-06, and
past that date `/status` reports `CG-517`. `deploy/fetch-data.sh` does the download.

**Source and credit:** IERS Earth Orientation Centre / Rapid Service–Prediction Centre (USNO),
<https://datacenter.iers.org/> (product "finals2000A.daily"). IERS products are freely available
for scientific and other use with acknowledgement of the IERS. This is public geodetic reference
data, not spacecraft or mission data (`AGENTS.md`).
