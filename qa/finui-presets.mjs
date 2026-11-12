// FinUI's preset overlay, made from FinUI's own create/preset.js and create/presets.json exactly as FinUI's
// tools/build-site.mjs makes it for its site: one file of tokens per option, p/<axis>/<option>.css, none for an axis'
// first option (tokens.css itself). Freightdock joins them into /assets/finui.css (appearance.rs). After copying a new
// FinUI, make them again:  node qa/finui-presets.mjs web/assets/finui web/assets/finui
// qa/js-check.sh makes them into a scratch folder and fails when they differ from the copy.
import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const [finui, out] = process.argv.slice(2);
if (!finui || !out) { console.error('node qa/finui-presets.mjs <finui folder> <out folder>'); process.exit(2); }
const { tokens, faces } = await import(pathToFileURL(path.resolve(finui, 'create/preset.js')).href);
const presets = JSON.parse(fs.readFileSync(path.join(finui, 'create/presets.json'), 'utf8'));
fs.rmSync(path.join(out, 'p'), { recursive: true, force: true });
presets.axes.forEach((axis, a) => axis.options.forEach((option, o) => {
  if (!o) return;
  const choice = presets.axes.map((_, i) => (i === a ? o : 0));
  const set = tokens(presets, choice);
  const f = path.join(out, `p/${a}/${o}.css`);
  fs.mkdirSync(path.dirname(f), { recursive: true });
  fs.writeFileSync(f, `/* ${axis.label}: ${option.label} */\n${faces(presets, choice)}:root {\n${set.map(([t, v]) => `  ${t}: ${v};\n`).join('')}}\n`);
}));
