import { emptyState } from '../../finui/components/empty/empty.js';
import { button } from '../../finui/components/button/button.js';
import { t } from '../i18n.js';

export function notFoundPage({ el }) {
  el.append(emptyState(t('view.nothing_here'), t('notfound.body'), button({ href: '/home', variant: 'primary' }, t('nav.home'))));
}
