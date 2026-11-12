// Cleaning up: Docker's system prune, asked first, with what it will take.

import { h, bytes } from '../dom.js';
import { post } from '../api.js';
import { base } from '../docker.js';
import { confirm } from '../confirm.js';
import { toast } from '../../finui/components/toast/toast.js';
import { t } from '../i18n.js';

export async function prune(after = () => {}) {
  const ok = await confirm({
    title: t('pages.prune.title'), action: t('common.clean_up'),
    body: h('div', { class: 'stack' }, h('p', null, t('pages.prune.body')),
      h('p', { class: 'muted' }, t('pages.prune.volumes_stay'))),
    option: { label: t('pages.prune.all'), help: t('pages.prune.all_help') },
  });
  if (!ok) return;
  try {
    const r = await post(base('/prune'), { all: ok.option, volumes: ok.option });
    toast({ text: t('pages.prune.done', { size: bytes(r.freed) }), tone: 'good' });
    after();
  } catch (e) { toast({ text: e.message, tone: 'critical' }); }
}
