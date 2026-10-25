// Open MCT bootstrap for ChronusGateway (Tranche 3, Methodology D-049).
import ChronusPlugin from './chronus-plugin.js';

const openmct = window.openmct;
const MINUTE = 60 * 1000;

openmct.install(openmct.plugins.LocalStorage());
openmct.install(openmct.plugins.MyItems());
openmct.install(openmct.plugins.Espresso());
openmct.install(openmct.plugins.UTCTimeSystem());
openmct.install(openmct.plugins.LADTable());
openmct.install(openmct.plugins.Clock({ enableClockIndicator: true }));
openmct.install(
  openmct.plugins.Conductor({
    menuOptions: [
      {
        name: 'Realtime',
        timeSystem: 'utc',
        clock: 'local',
        clockOffsets: { start: -15 * MINUTE, end: 15 * 1000 }
      },
      {
        name: 'Fixed',
        timeSystem: 'utc',
        bounds: { start: Date.now() - 30 * MINUTE, end: Date.now() }
      }
    ]
  })
);
openmct.install(ChronusPlugin());

document.addEventListener('DOMContentLoaded', () => openmct.start());
