/**
 * Open MCT plugin for ChronusGateway (Tranche 3, Methodology D-049).
 *
 * Everything is generated from the gateway's telemetry dictionary
 * (GET /api/v1/chronus/openmct/dictionary), so new points appear without changing this file:
 *
 *  - object tree: one folder named after the tracked spacecraft, one object per measurement
 *  - history:     GET /api/v1/chronus/openmct/history/{key}?start=&end=  (ms since the epoch)
 *  - realtime:    one shared WebSocket to /telemetry/openmct (openmct.realtime.v1); each message is
 *                 projected onto every subscribed point via the measurement's `chronus_field`
 *  - limits:      points flagged `chronus_alarm_when_nonzero` (the physics alarm bitfields) go red
 *                 when non-zero
 *
 * Talks to the gateway same-origin (see vite.config.js for the proxy).
 */

const NAMESPACE = 'chronus.taxonomy';
const ROOT_KEY = 'spacecraft';
const TYPE = 'chronus.telemetry';
const RECONNECT_MS = 2000;

const RED_ALARM = {
  cssClass: 'is-limit--upr is-limit--red',
  name: 'Physics alarm',
  low: 1,
  high: Number.POSITIVE_INFINITY
};

export default function ChronusPlugin(options = {}) {
  const base = options.baseUrl ?? '';

  return function install(openmct) {
    let dictionary;
    const getDictionary = () => {
      dictionary ??= fetch(`${base}/api/v1/chronus/openmct/dictionary`).then((r) => {
        if (!r.ok) {
          dictionary = undefined; // retry on the next request
          throw new Error(`dictionary request failed: HTTP ${r.status}`);
        }
        return r.json();
      });
      return dictionary;
    };

    // ---- objects ------------------------------------------------------------------------
    openmct.types.addType(TYPE, {
      name: 'Chronus telemetry point',
      description: 'A field of the ChronusGateway openmct.realtime.v1 message.',
      cssClass: 'icon-telemetry'
    });

    openmct.objects.addRoot({ namespace: NAMESPACE, key: ROOT_KEY });

    openmct.objects.addProvider(NAMESPACE, {
      async get(identifier) {
        const dict = await getDictionary();
        if (identifier.key === ROOT_KEY) {
          return { identifier, name: dict.name, type: 'folder', location: 'ROOT' };
        }
        const m = dict.measurements.find((x) => x.key === identifier.key);
        if (!m) {
          return undefined;
        }
        return {
          identifier,
          name: m.name,
          type: TYPE,
          location: `${NAMESPACE}:${ROOT_KEY}`,
          telemetry: { values: m.values },
          chronusField: m.chronus_field,
          chronusAlarm: Boolean(m.values[0]?.chronus_alarm_when_nonzero)
        };
      }
    });

    openmct.composition.addProvider({
      appliesTo: (d) => d.identifier.namespace === NAMESPACE && d.type === 'folder',
      async load() {
        const dict = await getDictionary();
        return dict.measurements.map((m) => ({ namespace: NAMESPACE, key: m.key }));
      }
    });

    // ---- history --------------------------------------------------------------------------
    openmct.telemetry.addProvider({
      supportsRequest: (d) => d.type === TYPE,
      async request(d, opts = {}) {
        const params = new URLSearchParams();
        if (Number.isFinite(opts.start)) params.set('start', Math.floor(opts.start));
        if (Number.isFinite(opts.end)) params.set('end', Math.ceil(opts.end));
        const key = encodeURIComponent(d.identifier.key);
        const r = await fetch(`${base}/api/v1/chronus/openmct/history/${key}?${params}`, {
          signal: opts.signal
        });
        return r.ok ? r.json() : [];
      }
    });

    // ---- realtime -------------------------------------------------------------------------
    const subscribers = new Map(); // key -> { field, callbacks: Set }
    let socket;

    const connect = () => {
      const proto = window.location.protocol === 'https:' ? 'wss' : 'ws';
      socket = new WebSocket(`${proto}://${window.location.host}${base}/telemetry/openmct`);
      socket.onmessage = (event) => {
        let msg;
        try {
          msg = JSON.parse(event.data);
        } catch {
          return;
        }
        const timestamp = Date.parse(msg.received_at);
        for (const [key, sub] of subscribers) {
          let value = msg[sub.field];
          if (typeof value === 'boolean') value = value ? 1 : 0;
          if (typeof value !== 'number') continue;
          const datum = { id: key, timestamp, value };
          sub.callbacks.forEach((cb) => cb(datum));
        }
      };
      socket.onclose = () => {
        if (subscribers.size > 0) setTimeout(connect, RECONNECT_MS);
        else socket = undefined;
      };
    };

    openmct.telemetry.addProvider({
      supportsSubscribe: (d) => d.type === TYPE,
      subscribe(d, callback) {
        const key = d.identifier.key;
        if (!subscribers.has(key)) {
          subscribers.set(key, { field: d.chronusField, callbacks: new Set() });
        }
        subscribers.get(key).callbacks.add(callback);
        if (!socket) connect();
        return () => {
          const sub = subscribers.get(key);
          sub?.callbacks.delete(callback);
          if (sub && sub.callbacks.size === 0) subscribers.delete(key);
          if (subscribers.size === 0 && socket) socket.close();
        };
      }
    });

    // ---- limits ---------------------------------------------------------------------------
    openmct.telemetry.addProvider({
      supportsLimits: (d) => d.type === TYPE && d.chronusAlarm,
      getLimitEvaluator() {
        return {
          evaluate: (datum) => (datum.value > 0 ? RED_ALARM : undefined)
        };
      }
    });
  };
}
