/** Chronus Open MCT WebSocket envelope (`chronus_schema: "openmct.realtime.v1"`). */
interface OpenMctRealtimeV1 {
  chronus_schema: string;
  apid: number;
  seq_count: number;
  received_at: string;
  physics_flags: number;
  source: string;
  elevation_deg?: number | null;
  azimuth_deg?: number | null;
  range_km?: number | null;
  range_rate_km_s?: number | null;
  payload_base64: string;
}

/** Bit semantics aligned with `crates/gateway/src/validate.rs` + `Methodology.md` D-016. */
const FLAG_BITS: readonly { mask: number; label: string }[] = [
  { mask: 0x01, label: "Doppler anomaly" },
  { mask: 0x02, label: "Below min elevation" },
  { mask: 0x04, label: "Link budget" },
  { mask: 0x08, label: "Pointing residual" },
  { mask: 0x10, label: "EPS (HIL)" },
  { mask: 0x20, label: "Thermal (HIL)" },
  { mask: 0x40, label: "ADCS body rate (HIL)" },
];

function defaultWsUrl(): string {
  const q = new URLSearchParams(window.location.search).get("ws");
  if (q) return q;
  const env = import.meta.env.VITE_GATEWAY_WS as string | undefined;
  if (env) return env;
  return "ws://127.0.0.1:8080/telemetry/openmct";
}

function fmtNum(n: number | null | undefined): string {
  if (n === null || n === undefined) return "—";
  if (typeof n !== "number" || Number.isNaN(n)) return "—";
  return Number.isInteger(n) ? String(n) : n.toFixed(4);
}

function renderFlags(flags: number): void {
  const raw = document.getElementById("flags-raw")!;
  const wrap = document.getElementById("flag-badges")!;
  raw.textContent = `0x${flags.toString(16).padStart(2, "0")} (${flags})`;
  wrap.replaceChildren();

  if (flags === 0) {
    const el = document.createElement("span");
    el.className = "badge badge--clear";
    el.textContent = "No physics alarms";
    wrap.appendChild(el);
    return;
  }

  for (const { mask, label } of FLAG_BITS) {
    if ((flags & mask) === 0) continue;
    const el = document.createElement("span");
    el.className = "badge badge--alarm";
    el.textContent = label;
    wrap.appendChild(el);
  }
}

function applyMessage(msg: OpenMctRealtimeV1, logEl: HTMLElement): void {
  document.getElementById("v-apid")!.textContent = String(msg.apid);
  document.getElementById("v-seq")!.textContent = String(msg.seq_count);
  document.getElementById("v-time")!.textContent = msg.received_at;
  document.getElementById("v-source")!.textContent = msg.source;
  document.getElementById("v-az")!.textContent = fmtNum(msg.azimuth_deg);
  document.getElementById("v-el")!.textContent = fmtNum(msg.elevation_deg);
  document.getElementById("v-range")!.textContent = fmtNum(msg.range_km);
  document.getElementById("v-rr")!.textContent = fmtNum(msg.range_rate_km_s);
  renderFlags(msg.physics_flags);

  const line = `[${msg.received_at}] apid=${msg.apid} seq=${msg.seq_count} flags=0x${msg.physics_flags.toString(16)}`;
  const prev = logEl.textContent?.trim();
  logEl.textContent = prev ? `${line}\n${prev}` : line;
  const lines = logEl.textContent.split("\n").slice(0, 40);
  logEl.textContent = lines.join("\n");
}

let socket: WebSocket | null = null;

function setConn(kind: "idle" | "connecting" | "live" | "error", text: string): void {
  const el = document.getElementById("conn-status")!;
  el.className = `status status--${kind}`;
  el.textContent = text;
}

function connect(): void {
  const input = document.getElementById("ws-url") as HTMLInputElement;
  const url = input.value.trim();
  if (!url) {
    setConn("error", "Enter a WebSocket URL");
    return;
  }
  socket?.close();
  setConn("connecting", "Connecting…");
  const ws = new WebSocket(url);
  socket = ws;
  const logEl = document.getElementById("log")!;

  ws.onopen = () => {
    setConn("live", "Connected");
    (document.getElementById("btn-connect") as HTMLButtonElement).disabled = true;
    (document.getElementById("btn-disconnect") as HTMLButtonElement).disabled = false;
  };

  ws.onmessage = (ev: MessageEvent<string>) => {
    try {
      const msg = JSON.parse(ev.data) as OpenMctRealtimeV1;
      if (msg.chronus_schema !== "openmct.realtime.v1") {
        logEl.textContent = `unknown schema: ${String(msg.chronus_schema)}\n${logEl.textContent ?? ""}`.slice(0, 4000);
        return;
      }
      applyMessage(msg, logEl);
    } catch {
      logEl.textContent = `parse error\n${logEl.textContent ?? ""}`.slice(0, 4000);
    }
  };

  ws.onerror = () => {
    setConn("error", "WebSocket error");
  };

  ws.onclose = () => {
    socket = null;
    (document.getElementById("btn-connect") as HTMLButtonElement).disabled = false;
    (document.getElementById("btn-disconnect") as HTMLButtonElement).disabled = true;
    setConn("idle", "Disconnected");
  };
}

function disconnect(): void {
  socket?.close();
  socket = null;
  setConn("idle", "Disconnected");
  (document.getElementById("btn-connect") as HTMLButtonElement).disabled = false;
  (document.getElementById("btn-disconnect") as HTMLButtonElement).disabled = true;
}

const wsInput = document.getElementById("ws-url") as HTMLInputElement;
wsInput.value = defaultWsUrl();

document.getElementById("btn-connect")!.addEventListener("click", () => connect());
document.getElementById("btn-disconnect")!.addEventListener("click", () => disconnect());
