// A QR code drawn as SVG from the encoder beside it (qr-encode.js): one path of dark modules on a light square, with a
// quiet zone of four. Always dark on light, whatever the theme (color-scheme: light in app.css), as some scanners
// read nothing else; it has a name for screen readers, and whatever it holds is shown as text beside it as well.

import { s } from '../../dom.js';
import { qrModules } from './qr-encode.js';
import { t } from '../../i18n.js';

export function qrCode(text, { label = t('components.qr_code.label'), size = 196 } = {}) {
  const m = qrModules(text);
  const n = m.length + 8;
  let d = '';
  m.forEach((row, y) => row.forEach((dark, x) => { if (dark) d += `M${x + 4} ${y + 4}h1v1h-1z`; }));
  return s('svg', { class: 'fd-qr', viewBox: `0 0 ${n} ${n}`, width: size, height: size, role: 'img', 'aria-label': label, 'shape-rendering': 'crispEdges' },
    s('rect', { class: 'fd-qr__light', width: n, height: n }),
    s('path', { class: 'fd-qr__dark', d }));
}
