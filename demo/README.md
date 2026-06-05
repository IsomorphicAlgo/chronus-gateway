# Demo & showcase assets (workspace root)

This directory is the **intended home** for Docker Compose files, dashboard sources (Open MCT bridge,
static SPA), and other **booth / portfolio** materials tracked in [`docs/SHOWCASE_PLAN.md`](../docs/SHOWCASE_PLAN.md).

**Why here:** `cargo publish` only packages each crate’s own folder (`crates/gateway/`,
`crates/chronus-hil-sim/`). Keeping demos under **workspace root** keeps the **crates.io tarball**
free of large or fast-moving showcase files (`Methodology.md` **D-025**).

**Separate download:** Optional **GitHub Release** zip (demo + short README) can be built in CI so
users fetch the showcase **without** pulling the full monorepo — see *Crates.io vs showcase
distribution* in `SHOWCASE_PLAN.md`.
