// An environment's or a group's security settings ("Access" 12 to 17): what everybody below Environment administrator
// may not make a container with there, and whether they may open consoles. An environment follows its group until it
// has settings of its own, which then hold whole. Administrators only.

import { h } from './dom.js';
import { get, put } from './api.js';
import { openModal } from '../finui/components/modal/modal.js';
import { button } from '../finui/components/button/button.js';
import { callout } from '../finui/components/callout/callout.js';
import { toggle } from '../finui/components/toggle/toggle.js';
import { settingRow } from '../finui/components/setting-row/setting-row.js';
import { segmented } from '../finui/components/segmented/segmented.js';
import { textArea, inlineError } from '../finui/components/field/field.js';
import { toast } from '../finui/components/toast/toast.js';
import { spinner } from '../finui/components/spinner/spinner.js';
import { t } from './i18n.js';

/** Each switch: its key, its words, and what it stops. */
export const SWITCHES = [
  ['block_privileged', t('security.switches.block_privileged.label'), t('security.switches.block_privileged.help')],
  ['block_bind_mounts', t('security.switches.block_bind_mounts.label'), t('security.switches.block_bind_mounts.help')],
  ['block_host_network', t('security.switches.block_host_network.label'), 'network_mode: host.'],
  ['block_host_pid', t('security.switches.block_host_pid.label'), t('security.switches.block_host_pid.help')],
  ['block_host_ipc', t('security.switches.block_host_ipc.label'), 'ipc: host.'],
  ['block_host_uts', t('security.switches.block_host_uts.label'), 'uts: host.'],
  ['block_devices', t('security.switches.block_devices.label'), t('security.switches.block_devices.help')],
  ['block_cap_add', t('security.switches.block_cap_add.label'), t('security.switches.block_cap_add.help')],
  ['block_security_opt', t('security.switches.block_security_opt.label'), t('security.switches.block_security_opt.help')],
  ['block_sysctls', t('security.switches.block_sysctls.label'), t('security.switches.block_sysctls.help')],
  ['block_consoles', t('security.switches.block_consoles.label'), t('security.switches.block_consoles.help')],
];

const HELP = t('security.help');

/** The dialog for `/api/environments/{id}/security` (`group` false) or `/api/environment-groups/{id}/security`. */
export function securityDialog({ title, path, group = false }) {
  const body = h('div', { class: 'stack' }, spinner(16));
  const m = openModal({ title, wide: true, body });
  get(path).then((d) => {
    const inherits = !group && d.group;
    // An environment with no settings of its own follows its group's (or, in no group, allows everything).
    let mode = group ? 'own' : d.own ? 'own' : 'follow';
    let rules = structuredClone((group ? d.rules : d.own || d.rules) || d.presets.open);
    const switchesSlot = h('div', { class: 'stack' });
    const allow = textArea({ id: 'bind-allow', label: t('security.bind_allow'), rows: 3, help: t('security.bind_allow_help') });
    allow.input.classList.add('mono');
    const allowError = h('div');
    const draw = () => {
      const own = mode === 'own';
      allow.input.value = (rules.bind_allow || []).join('\n');
      allow.input.disabled = !own;
      switchesSlot.replaceChildren(
        ...SWITCHES.map(([key, label, help]) => {
          const sw = toggle({ checked: !!rules[key], labelledby: `sec-${key}-label`, onChange: (v) => { rules[key] = v; } });
          sw.disabled = !own;
          return settingRow({ id: `sec-${key}`, label: t('security.refuse', { label }), help, control: sw });
        }).flat().filter(Boolean),
        allow.el, allowError);
    };
    const preset = (which) => { rules = structuredClone(d.presets[which]); mode = 'own'; if (modeSel) modeSel.setValue('own'); draw(); };
    let modeSel = null;
    const top = [];
    if (!group) {
      const follow = d.group ? t('security.follow', { name: d.group.name }) : t('security.allow_everything');
      modeSel = segmented({ label: t('security.settings'), size: 'sm', value: mode, options: [{ value: 'follow', label: follow }, { value: 'own', label: t('security.its_own') }],
        onChange: (v) => { mode = v; if (v === 'follow') rules = structuredClone((d.group && d.group.rules) || d.presets.open); draw(); } });
      top.push(settingRow({ id: 'sec-mode', label: t('security.settings'), help: inherits ? t('security.inherits', { name: d.group.name }) : t('security.no_group'), control: modeSel }));
    }
    const save = button({ variant: 'primary', onClick: async () => {
      const out = mode === 'own' ? { ...rules, bind_allow: allow.input.value.split('\n').map((s) => s.trim()).filter(Boolean) } : null;
      save.disabled = true; allowError.replaceChildren();
      try { await put(path, { rules: out }); toast({ text: t('common.saved'), tone: 'good' }); m.close(); }
      catch (e) { if (e.field === 'bind_allow') allowError.replaceChildren(inlineError('bind-allow-error', e.message)); else toast({ text: e.message, tone: 'critical' }); }
      finally { save.disabled = false; }
    } }, t('common.save'));
    draw();
    body.replaceChildren(callout({ tone: 'info', body: HELP }), ...top.flat().filter(Boolean),
      h('div', { class: 'row' },
        button({ size: 'sm', onClick: () => preset('locked_down') }, t('security.locked_down')),
        button({ size: 'sm', variant: 'ghost', onClick: () => preset('open') }, t('security.allow_everything'))),
      switchesSlot,
      h('div', { class: 'row row--end' }, save));
  }, (e) => body.replaceChildren(callout({ tone: 'critical', body: e.message })));
}
