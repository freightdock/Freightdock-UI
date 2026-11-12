// The app's frame: FinUI's desktop menu (Grouped) beside the page on a wide screen, its mobile menu (the tab bar) and a
// top bar on a phone (the host is chosen on Home only, "UI" 14). Built once per signed-in person and kept across
// pages; the router only swaps what is in `content`. The menus are made again, in place, when the host chosen changes
// or becomes, or stops being, a swarm manager: its pages are listed under its name (its Swarm group only for a
// manager), and while none is chosen only Home and what is Freightdock's own are there.

import { h, mount, brand, brandMark } from './dom.js';
import { state, isAdmin, themeChoice, setTheme, hasUnseenVersion, onVersionSeen, setEnvs, onEnvChange, currentEnv } from './state.js';
import { onRouteChange } from './router.js';
import { get } from './api.js';
import { isSwarmManager, refreshSwarm, onSwarmChange } from './swarm.js';
import { clusterConnected, refreshCluster, onClusterChange } from './cluster.js';
import { desktopNav, STYLES as DESKTOP } from '../finui/components/desktop-nav/desktop-nav.js';
import { mobileNav, STYLES as MOBILE } from '../finui/components/mobile-nav/mobile-nav.js';
import { themeSwitch } from '../finui/components/theme-switch/theme-switch.js';
import { avatar } from '../finui/components/avatar/avatar.js';
import { t } from './i18n.js';

const DESKTOP_STYLE = 'grouped';
const MOBILE_STYLE = 'tabs';

function pages() {
  const env = state.env ? currentEnv() : null;
  const host = state.env ? t('nav.host', { name: env ? env.name : '…' }) : null;
  return [
    { href: '/home', label: t('nav.home'), icon: 'globe', group: t('nav.overview'), primary: true, exact: true },
    clusterConnected() ? { href: '/cluster', label: t('nav.the_cluster'), icon: 'compass', group: t('nav.overview'), primary: true, exact: true } : null,
    clusterConnected() ? { href: '/cluster/stacks', label: t('nav.cluster_stacks'), icon: 'grid', group: t('nav.overview'), primary: true } : null,
    ...(host ? [
      { href: '/', label: t('nav.dashboard'), icon: 'home', group: host, primary: true, exact: true },
      { href: '/containers', label: t('nav.containers'), icon: 'package', group: host, primary: true },
      { href: '/stacks', label: t('nav.stacks'), icon: 'grid', group: host, primary: true },
      { href: '/images', label: t('nav.images'), icon: 'layers', group: host, primary: true },
      { href: '/templates', label: t('nav.templates'), icon: 'rocket', group: host },
      { href: '/volumes', label: t('nav.volumes'), icon: 'database', group: host },
      { href: '/networks', label: t('nav.networks'), icon: 'lan', group: host },
      { href: '/events', label: t('nav.events'), icon: 'activity', group: host },
    ] : [{ key: 'no-host', href: '/home', label: t('nav.no_host'), icon: 'server', group: t('nav.overview') }]),
    ...(host && isSwarmManager() ? [
      { href: '/swarm', label: t('nav.cluster'), icon: 'compass', group: t('nav.swarm'), exact: true },
      { href: '/swarm/services', label: t('nav.services'), icon: 'zap', group: t('nav.swarm') },
      { href: '/swarm/stacks', label: t('nav.swarm_stacks'), icon: 'listChecks', group: t('nav.swarm') },
      { href: '/swarm/nodes', label: t('nav.nodes'), icon: 'hardDrive', group: t('nav.swarm') },
      { href: '/swarm/secrets', label: t('nav.secrets'), icon: 'key', group: t('nav.swarm') },
      { href: '/swarm/configs', label: t('nav.configs'), icon: 'fileText', group: t('nav.swarm') },
    ] : []),
    { href: '/environments', label: t('nav.environments'), icon: 'server', group: t('nav.freightdock') },
    { href: '/registries', label: t('nav.registries'), icon: 'cloud', group: t('nav.freightdock') },
    isAdmin() ? { href: '/users', label: t('nav.users'), icon: 'users', group: t('nav.freightdock') } : null,
    isAdmin() ? { href: '/activity', label: t('nav.activity'), icon: 'log', group: t('nav.freightdock') } : null,
    isAdmin() ? { href: '/auth-log', label: t('nav.auth_log'), icon: 'login', group: t('nav.freightdock') } : null,
    isAdmin() ? { href: '/notifications', label: t('nav.notifications'), icon: 'bell', group: t('nav.freightdock') } : null,
    isAdmin() ? { href: '/backups', label: t('nav.backups'), icon: 'save', group: t('nav.freightdock') } : null,
    isAdmin() ? { href: '/settings', label: t('nav.settings'), icon: 'settings', group: t('nav.freightdock') } : null,
    { href: '/changelog', label: t('nav.patch_notes'), icon: 'tag', group: t('nav.freightdock'), dot: hasUnseenVersion },
    { href: '/account', label: t('nav.account'), icon: 'user', group: t('nav.freightdock') },
  ].filter(Boolean).map((p) => ({ ...p, key: p.key || p.href }));
}

/** What the menus depend on of the host: which one, its name, and whether it is a swarm manager. */
const hostKey = () => `${clusterConnected() ? 'cluster' : ''}|` + (state.env ? `${state.env}:${(currentEnv() || {}).name || ''}:${isSwarmManager() ? 'swarm' : ''}` : '');

/** The menu entry a path belongs to: its own, or the one it lies under (/containers/abc is Containers). */
export function currentKey(path, list = pages()) {
  const exact = list.find((p) => p.href === path);
  if (exact) return exact.key;
  const under = list.filter((p) => !p.exact && path.startsWith(p.href + '/')).sort((a, b) => b.href.length - a.href.length)[0];
  return under ? under.key : null;
}

let shell = null;   // { el, content, setCurrent, destroy, userId }

/** Asks for the environments again (their health changes), quietly: the menus keep what they had on a failure. */
export async function refreshEnvs() {
  try { setEnvs(await get('/api/environments')); } catch { /* the page says why */ }
}
function build() {
  const me = state.user;
  const who = () => avatar(me.avatar || null, me.name, { size: 26 });
  const theme = () => themeSwitch({ value: themeChoice(), onChange: setTheme });
  const deskSlot = h('div', { class: 'desktop-nav' }), mobSlot = h('div', { class: 'mobile-nav' }), triggerSlot = h('span', { class: 'topbar-menu' });
  let navs = null, navKey = null;
  /** The two menus for the host chosen now; made again only when that changed. */
  function menus() {
    const key = hostKey();
    if (navs && key === navKey) return;
    if (navs) { navs.desktop.destroy(); navs.mobile.destroy(); }
    navKey = key;
    const list = pages();
    const desktop = desktopNav({
      style: DESKTOP_STYLE, pages: list, brand: { href: '/home', mark: brandMark(22), name: 'Freightdock' },
      me: { href: '/account', avatar: who(), name: me.name, role: me.role === 'admin' ? t('common.administrator') : t('nav.user') },
      theme: theme(), label: t('nav.pages'),
    });
    const mobile = mobileNav({ style: MOBILE_STYLE, pages: list, label: t('nav.menu'),
      foot: h('div', { class: 'mobile-nav-foot' }, h('a', { class: 'me', href: '/account' }, who(), h('span', { class: 'me-name' }, me.name)), theme()) });
    mount(deskSlot, desktop.el); mount(mobSlot, mobile.el); mount(triggerSlot, mobile.trigger);
    navs = { desktop, mobile, list };
    mark(location.pathname);
  }
  function mark(path) { const k = currentKey(path, navs.list); navs.desktop.setCurrent(k); navs.mobile.setCurrent(k); }
  menus();
  const topbar = h('header', { class: 'topbar' }, triggerSlot, h('a', { href: '/home', class: 'topbar-brand' }, brand(22)), h('span', { class: 'topbar-end' }));
  const content = h('div', { class: 'content', id: 'content' });
  const main = h('main', { class: 'main', tabindex: -1 }, content);
  const size = DESKTOP.find((s) => s.key === DESKTOP_STYLE).size;
  const space = MOBILE.find((s) => s.key === MOBILE_STYLE).space;
  const el = h('div', { class: ['app', 'nav-left', space > 0 && 'has-mobile-dock'] }, deskSlot, topbar, mobSlot, main);
  el.style.setProperty('--desktop-nav-size', `${size}px`);
  el.style.setProperty('--mobile-nav-space', `${space}px`);
  const stop = onRouteChange((path) => { menus(); mark(path); refreshEnvs(); if (state.env) refreshSwarm(); });
  const stopDots = onVersionSeen(() => { navs.desktop.repaint(); navs.mobile.repaint(); });
  const stopEnvs = onEnvChange(() => { menus(); });
  const stopSwarm = onSwarmChange(menus);
  const stopCluster = onClusterChange(menus);
  refreshCluster();
  return {
    el, content, userId: me.id,
    destroy() { stop(); stopDots(); stopEnvs(); stopSwarm(); stopCluster(); navs.desktop.destroy(); navs.mobile.destroy(); },
  };
}

/** The shell's content, emptied for the next page. */
export function shellContent() {
  const root = document.getElementById('app');
  if (!shell || shell.userId !== (state.user && state.user.id) || !root.contains(shell.el)) {
    if (shell) shell.destroy();
    shell = build();
    mount(root, shell.el);
  }
  mount(shell.content);
  return shell.content;
}

/** A page without the shell: setup and sign-in. */
export function bareContent() {
  if (shell) { shell.destroy(); shell = null; }
  const el = h('div', { class: 'bare' });
  mount(document.getElementById('app'), el);
  return el;
}

/** Throws the shell away, so the next page builds it again (after signing in as somebody else, or out). */
export function resetShell() {
  if (shell) { shell.destroy(); shell = null; }
}
