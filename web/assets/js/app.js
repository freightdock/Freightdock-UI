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
import { dashboardPage } from './pages/dashboard.js';
import { activityPage } from './pages/activity.js';
import { settingsPage } from './pages/settings.js';
import { changelogPage } from './pages/changelog.js';
import { containersPage } from './pages/containers.js';
import { containerPage } from './pages/container.js';
import { containerEditPage } from './pages/container-edit.js';
import { imagesPage, imagePage, buildPage } from './pages/images.js';
import { volumesPage } from './pages/volumes.js';
import { volumeFilesPage } from './pages/volume-files.js';
import { networksPage } from './pages/networks.js';
import { eventsPage } from './pages/events.js';
import { environmentsPage } from './pages/environments.js';
import { stacksPage, newStackPage, externalStackPage } from './pages/stacks.js';
import { stackPage } from './pages/stack.js';
import { templatesPage, customTemplatePage } from './pages/templates.js';
import { homePage } from './pages/home.js';
import { registriesPage, registryPage } from './pages/registries.js';
import { clusterPage, nodesPage } from './pages/swarm.js';
import { servicesPage, servicePage } from './pages/swarm-services.js';
import { stacksPage as swarmStacksPage, newStackPage as newSwarmStackPage, stackPage as swarmStackPage } from './pages/swarm-stacks.js';
import { secretsPage, configsPage } from './pages/swarm-objects.js';
import { notificationsPage } from './pages/notifications.js';
import { backupsPage } from './pages/backups.js';
import { portainerPage } from './pages/portainer.js';
import { usersPage } from './pages/users.js';
import { signinsPage } from './pages/signins.js';
import { theClusterPage } from './pages/cluster.js';
import { clusterStacksPage } from './pages/cluster-stacks.js';
import { t } from './i18n.js';

motion();
animateWithin(document.body);

route('/setup', setupPage, { bare: true, title: t('routes.setup') });
route('/login', loginPage, { bare: true, title: t('routes.login') });
route('/home', homePage, { title: t('nav.home') });
route('/cluster', theClusterPage, { title: t('nav.the_cluster') });
route('/cluster/stacks', clusterStacksPage, { title: t('nav.cluster_stacks') });
route('/', dashboardPage, { title: t('nav.dashboard'), host: true });
route('/account', accountPage, { title: t('nav.account') });
route('/activity', activityPage, { title: t('nav.activity') });
route('/users', usersPage, { title: t('nav.users') });
route('/auth-log', signinsPage, { title: t('nav.auth_log') });
route('/settings', settingsPage, { title: t('nav.settings') });
route('/notifications', notificationsPage, { title: t('nav.notifications') });
route('/backups', backupsPage, { title: t('nav.backups') });
route('/changelog', changelogPage, { title: t('nav.patch_notes') });
route('/containers', containersPage, { title: t('nav.containers'), host: true });
route('/containers/new', containerEditPage, { title: t('routes.add_container'), host: true });
route('/containers/:id/edit', containerEditPage, { title: (p) => t('routes.edit', { name: p.id }), host: true });
route('/containers/:id', containerPage, { title: (p) => p.id, host: true });
route('/stacks', stacksPage, { title: t('nav.stacks'), host: true });
route('/stacks/new', newStackPage, { title: t('routes.add_stack'), host: true });
route('/stacks/external/:name', externalStackPage, { title: (p) => p.name, host: true });
route('/stacks/:id', stackPage, { title: t('routes.stack'), host: true });
route('/templates', templatesPage, { title: t('nav.templates'), host: true });
route('/templates/custom/new', customTemplatePage, { title: t('routes.new_template'), host: true });
route('/templates/custom/:id', customTemplatePage, { title: t('routes.template'), host: true });
route('/images', imagesPage, { title: t('nav.images'), host: true });
route('/images/build', buildPage, { title: t('routes.build'), host: true });
route('/images/:id', imagePage, { title: t('routes.image'), host: true });
route('/volumes', volumesPage, { title: t('nav.volumes'), host: true });
route('/volumes/:name/files', volumeFilesPage, { title: (p) => t('routes.volume_files', { name: p.name }), host: true });
route('/networks', networksPage, { title: t('nav.networks'), host: true });
route('/events', eventsPage, { title: t('nav.events'), host: true });
route('/environments', environmentsPage, { title: t('nav.environments') });
route('/registries', registriesPage, { title: t('nav.registries') });
route('/registries/:id', registryPage, { title: t('routes.registry') });
route('/swarm', clusterPage, { title: t('nav.swarm'), host: true });
route('/swarm/nodes', nodesPage, { title: t('nav.nodes'), host: true });
route('/swarm/services', servicesPage, { title: t('nav.services'), host: true });
route('/swarm/services/:id', servicePage, { title: (p) => p.id, host: true });
route('/swarm/stacks', swarmStacksPage, { title: t('nav.swarm_stacks'), host: true });
route('/swarm/stacks/new', newSwarmStackPage, { title: t('routes.deploy_stack'), host: true });
route('/swarm/stacks/:name', swarmStackPage, { title: (p) => p.name, host: true });
route('/swarm/secrets', secretsPage, { title: t('nav.secrets'), host: true });
route('/swarm/configs', configsPage, { title: t('nav.configs'), host: true });
route('/portainer', portainerPage, { title: t('routes.portainer') });
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
