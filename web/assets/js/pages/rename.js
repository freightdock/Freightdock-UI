// Renaming a container: a small dialog with its name, the server's answer beside the field.

import { h } from '../dom.js';
import { post } from '../api.js';
import { base } from '../docker.js';
import { navigate } from '../router.js';
import { openModal } from '../../finui/components/modal/modal.js';
import { formField } from '../../finui/components/field/field.js';
import { button } from '../../finui/components/button/button.js';
import { toast } from '../../finui/components/toast/toast.js';
import { t } from '../i18n.js';

export function renameContainer(id, name) {
  const f = formField({ id: 'rename', label: t('pages.rename.new_name'), autocomplete: 'off' });
  f.input.value = name;
  const go = button({ variant: 'primary', type: 'submit' }, t('pages.rename.rename'));
  const form = h('form', { class: 'stack', novalidate: true }, f.el, h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: t('pages.rename.title', { name }), body: form, initialFocus: f.input });
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    f.setError('');
    go.disabled = true;
    try {
      await post(base(`/containers/${encodeURIComponent(id)}/rename`), { name: f.input.value.trim() });
      m.close();
      toast({ text: t('pages.rename.done', { old: name, name: f.input.value.trim() }), tone: 'good' });
      navigate(`/containers/${encodeURIComponent(f.input.value.trim())}`, { replace: true });
    } catch (err) { f.setError(err.message); } finally { go.disabled = false; }
  });
}
