// What setting up two-factor looks like, at sign-in (when it is required) and in Account alike: the QR code an
// authenticator app scans, the secret as text for one that cannot scan, a field for the code it then shows; and the
// recovery codes, shown once.

import { h } from '../dom.js';
import { qrCode } from '../components/qr-code/qr-code.js';
import { codeLine } from '../../finui/components/code-block/code-block.js';
import { callout } from '../../finui/components/callout/callout.js';
import { openModal } from '../../finui/components/modal/modal.js';
import { button } from '../../finui/components/button/button.js';
import { formField } from '../../finui/components/field/field.js';
import { t } from '../i18n.js';

/** A field for a six-digit code (or a recovery code, with `recovery`). */
export function codeField(id, { recovery = false } = {}) {
  return formField({ id, label: recovery ? t('two_factor.code') : t('two_factor.code_it_shows'), autocomplete: 'one-time-code', inputMode: recovery ? 'text' : 'numeric',
    help: recovery ? t('two_factor.code_help') : null });
}

/** The QR code and the secret for `{ secret, uri }`, and the code field under them. */
export function setupBox(made, code) {
  const grouped = made.secret.replace(/(.{4})/g, '$1 ').trim();
  return h('div', { class: 'fd-totp-setup' },
    qrCode(made.uri, { label: t('two_factor.qr') }),
    h('div', { class: 'stack' },
      h('p', null, t('two_factor.scan')),
      codeLine(grouped, { label: t('two_factor.copy_key') }),
      code.el));
}

/** The recovery codes, once: each signs in once in place of a code, for when the phone is lost. */
export function showRecoveryCodes(codes, { onClose } = {}) {
  const text = codes.join('\n');
  const copy = button({ size: 'sm', onClick: async () => { try { await navigator.clipboard.writeText(text); copy.replaceChildren(t('two_factor.copied')); } catch { /* nothing to copy to */ } } }, t('two_factor.copy_them'));
  const done = button({ variant: 'primary', onClick: () => m.close() }, t('two_factor.kept'));
  const m = openModal({ title: t('two_factor.recovery_title'), onClose, body: h('div', { class: 'stack' },
    callout({ tone: 'warning', body: t('two_factor.recovery_keep') }),
    h('ul', { class: 'fd-recovery' }, ...codes.map((c) => h('li', null, c))),
    h('div', { class: 'row row--end' }, copy, done)), initialFocus: done });
  return m;
}
