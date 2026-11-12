// The app, once main.js has put the language in use: FinMotion, the moving icons, then who is signed in, then the first
// page.

import { motion } from '../finmotion/core/finmotion.js';
import { animateWithin } from '../finui/components/animated-icon/animated-icon.js';
import { get, setCsrf, setUnauthorizedHandler } from './api.js';
import { state } from './state.js';
import { route, setLayouts, start, navigate } from './router.js';
import { shellContent, bareContent, resetShell } from './shell.js';
import { h, mount } from './dom.js';
import { errorState } from '../finui/components/error/error.js';
import { setupPage } from './pages/setup.js';
import { loginPage } from './pages/login.js';
import { accountPage } from './pages/account.js';
import { notFoundPage } from './pages/notfound.js';
import { activityPage } from './pages/activity.js';
import { settingsPage } from './pages/settings.js';
import { changelogPage } from './pages/changelog.js';
import { usersPage } from './pages/users.js';
import { signinsPage } from './pages/signins.js';
import { theClusterPage } from './pages/cluster.js';
import { clusterStacksPage } from './pages/cluster-stacks.js';
import { clusterNodePage } from './pages/cluster-node.js';
import { clusterManagerPage } from './pages/cluster-manager.js';
import { dashboardPage } from './pages/dashboard.js';
import { t } from './i18n.js';

motion();
animateWithin(document.body);

route('/setup', setupPage, { bare: true, title: t('routes.setup') });
route('/login', loginPage, { bare: true, title: t('routes.login') });
route('/dashboard', dashboardPage, { title: t('pages.dashboard.title') });
route('/cluster', theClusterPage, { title: t('nav.the_cluster') });
route('/cluster/stacks', clusterStacksPage, { title: t('nav.cluster_stacks') });
route('/cluster/nodes/:id', clusterNodePage, { title: t('pages.cluster_node.title') });
route('/cluster/managers/:id', clusterManagerPage, { title: t('pages.cluster_manager.title') });
route('/account', accountPage, { title: t('nav.account') });
route('/activity', activityPage, { title: t('nav.activity') });
route('/users', usersPage, { title: t('nav.users') });
route('/auth-log', signinsPage, { title: t('nav.auth_log') });
route('/settings', settingsPage, { title: t('nav.settings') });
route('/changelog', changelogPage, { title: t('nav.patch_notes') });
route('/404', notFoundPage, { title: t('routes.not_found') });

setLayouts({ shell: () => shellContent(), bare: () => bareContent() });

/** Who is signed in now, asked of the server; null when nobody is. */
export async function refreshMe() {
  try {
    const me = await get('/api/me', { quiet401: true });
    state.user = me.user;
    setCsrf(me.csrf);
  } catch (e) {
    if (e.status !== 401) throw e;
    state.user = null;
    setCsrf('');
  }
  resetShell();
}

setUnauthorizedHandler(() => {
  if (!state.user) return;
  state.user = null;
  resetShell();
  navigate(location.pathname === '/' ? '/login' : '/login?next=' + encodeURIComponent(location.pathname + location.search), { replace: true });
});

async function boot() {
  try {
    state.status = await get('/api/status');
    if (state.status.configured) await refreshMe();
  } catch (e) {
    const root = document.getElementById('app');
    mount(root, h('div', { class: 'bare' }, h('div', { class: 'auth-card' }, errorState(e, () => location.reload()))));
    return;
  }
  start();
}

boot();
