// The app's frame: FinUI's desktop menu (Grouped) beside the page on a wide screen, its mobile menu (the tab bar) and a
// top bar on a phone. Built once per signed-in person and kept across pages; the router only swaps what is in
// `content`. The menus are made again, in place, when the cluster's managers start or stop answering.

import { h, mount, brand, brandMark } from './dom.js';
import { state, isAdmin, themeChoice, setTheme, hasUnseenVersion, onVersionSeen } from './state.js';
import { onRouteChange } from './router.js';
import { clusterConnected, refreshCluster, onClusterChange } from './cluster.js';
import { desktopNav, STYLES as DESKTOP } from '../finui/components/desktop-nav/desktop-nav.js';
import { mobileNav, STYLES as MOBILE } from '../finui/components/mobile-nav/mobile-nav.js';
import { themeSwitch } from '../finui/components/theme-switch/theme-switch.js';
import { avatar } from '../finui/components/avatar/avatar.js';
import { t } from './i18n.js';

const DESKTOP_STYLE = 'grouped';
const MOBILE_STYLE = 'tabs';

function pages() {
  const cluster = t('nav.overview');
  return [
    { href: '/dashboard', label: t('pages.dashboard.title'), icon: 'gauge', group: cluster, primary: true, exact: true },
    { href: '/cluster', label: t('nav.the_cluster'), icon: 'compass', group: cluster, primary: true, exact: true },
    clusterConnected() ? { href: '/cluster/stacks', label: t('nav.cluster_stacks'), icon: 'grid', group: cluster, primary: true } : null,
    clusterConnected() ? { href: '/cluster/network', label: t('nav.cluster_network'), icon: 'lan', group: cluster, primary: true } : null,
    isAdmin() ? { href: '/users', label: t('nav.users'), icon: 'users', group: t('nav.freightdock') } : null,
    isAdmin() ? { href: '/activity', label: t('nav.activity'), icon: 'log', group: t('nav.freightdock') } : null,
    isAdmin() ? { href: '/auth-log', label: t('nav.auth_log'), icon: 'login', group: t('nav.freightdock') } : null,
    isAdmin() ? { href: '/settings', label: t('nav.settings'), icon: 'settings', group: t('nav.freightdock') } : null,
    { href: '/changelog', label: t('nav.patch_notes'), icon: 'tag', group: t('nav.freightdock'), dot: hasUnseenVersion },
    { href: '/account', label: t('nav.account'), icon: 'user', group: t('nav.freightdock') },
  ].filter(Boolean).map((p) => ({ ...p, key: p.key || p.href }));
}

/** What the menus depend on: whether the managers answer. */
const menuKey = () => (clusterConnected() ? 'cluster' : '');

/** The menu entry a path belongs to: its own, or the one it lies under (/cluster/stacks/web is Stacks). */
export function currentKey(path, list = pages()) {
  const exact = list.find((p) => p.href === path);
  if (exact) return exact.key;
  const under = list.filter((p) => !p.exact && path.startsWith(p.href + '/')).sort((a, b) => b.href.length - a.href.length)[0];
  return under ? under.key : null;
}

let shell = null;   // { el, content, setCurrent, destroy, userId }

function build() {
  const me = state.user;
  const who = () => avatar(me.avatar || null, me.name, { size: 26 });
  const theme = () => themeSwitch({ value: themeChoice(), onChange: setTheme });
  const deskSlot = h('div', { class: 'desktop-nav' }), mobSlot = h('div', { class: 'mobile-nav' }), triggerSlot = h('span', { class: 'topbar-menu' });
  let navs = null, navKey = null;
  /** The two menus; made again only when what they show changed. */
  function menus() {
    const key = menuKey();
    if (navs && key === navKey) return;
    if (navs) { navs.desktop.destroy(); navs.mobile.destroy(); }
    navKey = key;
    const list = pages();
    const desktop = desktopNav({
      style: DESKTOP_STYLE, pages: list, brand: { href: '/cluster', mark: brandMark(22), name: 'Freightdock' },
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
  const topbar = h('header', { class: 'topbar' }, triggerSlot, h('a', { href: '/cluster', class: 'topbar-brand' }, brand(22)), h('span', { class: 'topbar-end' }));
  const content = h('div', { class: 'content', id: 'content' });
  const main = h('main', { class: 'main', tabindex: -1 }, content);
  const size = DESKTOP.find((s) => s.key === DESKTOP_STYLE).size;
  const space = MOBILE.find((s) => s.key === MOBILE_STYLE).space;
  const el = h('div', { class: ['app', 'nav-left', space > 0 && 'has-mobile-dock'] }, deskSlot, topbar, mobSlot, main);
  el.style.setProperty('--desktop-nav-size', `${size}px`);
  el.style.setProperty('--mobile-nav-space', `${space}px`);
  const stop = onRouteChange((path) => { menus(); mark(path); });
  const stopDots = onVersionSeen(() => { navs.desktop.repaint(); navs.mobile.repaint(); });
  const stopCluster = onClusterChange(menus);
  refreshCluster();
  return {
    el, content, userId: me.id,
    destroy() { stop(); stopDots(); stopCluster(); navs.desktop.destroy(); navs.mobile.destroy(); },
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
