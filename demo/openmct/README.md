# Open MCT bridge (Showcase **S2** — Track A — backlog)

[NASA Open MCT](https://github.com/nasa/openmct) is the long-term mission-control target referenced in the
gateway design (`chronus_schema: "openmct.realtime.v1"` on `GET /telemetry/openmct`).

**Track B** is implemented first: a minimal dashboard in [`../dashboard/`](../dashboard/) (Vite + TypeScript)
so demos work without cloning Open MCT.

When this Track A item is picked up, expected artifacts here (or in a sibling repo):

- A thin **adapter** or **bridge** page that opens a WebSocket to Chronus and maps JSON lines into Open MCT
  **telemetry objects** and **limits** driven by `physics_flags`.
- Documented Open MCT **release tag** and install steps pinned in [`../../docs/DEMO.md`](../../docs/DEMO.md).

Companion plan: [`../../docs/SHOWCASE_PLAN.md`](../../docs/SHOWCASE_PLAN.md) §S2.
