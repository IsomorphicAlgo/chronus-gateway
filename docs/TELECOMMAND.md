# Telecommand: out of scope for 1.x

*Decision recorded in 0.5.0 (Tranche 3, Methodology D-050).*

"TMTC" means telemetry **and** telecommand. ChronusGateway-RS handles telemetry only. It has
**no uplink path** and will not have one in 1.x:

- It never builds, queues, forwards or transmits a telecommand.
- A CCSDS packet with the TC type bit that arrives on the telemetry port is **rejected**, not
  passed on. It is counted as an invalid packet, and the log explains the first one (`CG-404`).
- The HTTP and WebSocket API is **read-only**. No endpoint accepts data that could reach a
  spacecraft.

## Why

Sending commands to a spacecraft is a different kind of system from watching its telemetry.
Getting telemetry wrong produces a wrong number on a screen. Getting commanding wrong can lose the
mission. Doing it responsibly needs, at minimum:

| Need | Why it matters |
|---|---|
| **Authenticated, authorised operators** | Telemetry can be public; commanding must be attributable to a person with the right role (and often two people). |
| **Command dictionary and validation** | Every command checked against a mission database (ranges, prerequisites, forbidden combinations) before it leaves the ground. |
| **Interlocks** | No transmission outside a pass, or when the spacecraft is in a state where the command is dangerous. The physics this gateway already computes would feed those checks, but deciding to transmit is a separate safety case. |
| **The uplink protocol stack** | TC Space Data Link Protocol (CCSDS 232.0-B), the COP-1 retransmission protocol driven by the CLCW in the telemetry OCF, CLTU coding (231.0-B), and usually SDLS security (355.0-B). |
| **Audit trail** | A tamper-evident record of what was sent, by whom, and what the spacecraft acknowledged. |
| **Regulatory and export posture** | Transmitting needs a licence for the band, and commanding a real spacecraft can bring in controlled technical data. The project's compliance posture keeps it to open standards and synthetic data. |

None of that is a feature you can add on the side, and a half-built version would be worse than
none. Keeping 1.x telemetry-only makes the security promise simple: **there is nothing to exploit
on the way up, because there is no way up.**

## What would a 2.x track look like

If commanding is ever added, it should be a **separate process and crate** (for example
`chronus-uplink`), not a mode of the telemetry gateway. Roughly:

1. Design document and safety case first (threats, interlocks, roles), then owner sign-off recorded
   in `Methodology.md`, as the project rules require for anything that changes its compliance
   posture.
2. A command dictionary format and a validator, tested against a simulator only.
3. TC frames, COP-1 (using the CLCW the telemetry side already sees in the OCF) and CLTU
   encoding, verified against the NeXosim HIL simulator, never a real spacecraft.
4. Authentication, two-person approval and an append-only audit log before any network exposure.

Until then, the honest description is **"a CCSDS telemetry gateway with physics co-validation."**
