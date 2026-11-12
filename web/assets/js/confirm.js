// A question before something that cannot be undone: FinUI's modal with the thing named, what will happen, and the
// button that does it in the danger look. Answers true or false.

import { h } from './dom.js';
import { openModal } from '../finui/components/modal/modal.js';
import { button } from '../finui/components/button/button.js';
import { toggle } from '../finui/components/toggle/toggle.js';
import { t } from './i18n.js';

/** confirm({ title, body, action, danger, option: { label, help } }) → false, or true (or { option } when there is one). */
export function confirm({ title, body, action = t('common.remove'), danger = true, option = null }) {
  return new Promise((resolve) => {
    let answered = false, chosen = false;
    const done = (v) => { if (answered) return; answered = true; m.close(); resolve(v); };
    const opt = option ? h('div', { class: 'fui-setting-row' },
      h('div', null, h('div', { class: 'fui-setting-row__label', id: 'confirm-opt' }, option.label), option.help ? h('p', { class: 'fui-field__help' }, option.help) : null),
      h('div', { class: 'fui-setting-row__control' }, toggle({ checked: false, labelledby: 'confirm-opt', onChange: (v) => { chosen = v; } }))) : null;
    const go = button({ variant: danger ? 'danger' : 'primary', onClick: () => done(option ? { option: chosen } : true) }, action);
    const m = openModal({
      title, initialFocus: go, onClose: () => { if (!answered) { answered = true; resolve(false); } },
      body: h('div', { class: 'stack' }, typeof body === 'string' ? h('p', null, body) : body, opt,
        h('div', { class: 'row row--end' }, button({ onClick: () => done(false) }, t('common.cancel')), go)),
    });
  });
}
