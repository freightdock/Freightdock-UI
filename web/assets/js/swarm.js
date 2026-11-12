// Swarm, as the pages read it: whether the host shown is a swarm manager (asked of /swarm, kept per environment for
// half a minute), the paths under it, and how nodes, tasks and services read.

import { h, mount } from './dom.js';
import { get } from './api.js';
import { base } from './docker.js';
import { state } from './state.js';
import { status } from '../finui/components/badge/badge.js';
import { emptyState } from '../finui/components/empty/empty.js';
import { t, tOr } from './i18n.js';

export const sbase = (p = '') => base(`/swarm${p}`);

const known = new Map();   // env -> { role, at }
const subs = new Set();
const FRESH_MS = 30000;

/** The role of the host shown, as last asked: 'manager', 'worker', 'none', or null before the answer. */
export const swarmRole = (env = state.env) => (known.get(env) || {}).role || null;
export const isSwarmManager = (env = state.env) => swarmRole(env) === 'manager';
export function onSwarmChange(fn) { subs.add(fn); return () => subs.delete(fn); }

/** Asks the host shown what it is to a swarm, unless that was asked within half a minute (`force` asks anyway). */
export async function refreshSwarm({ force = false } = {}) {
  const env = state.env;
  if (!env) return null;   // no host chosen: nothing to ask
  const was = known.get(env);
  if (!force && was && Date.now() - was.at < FRESH_MS) return was.role;
  let role = 'none';
  try { role = (await get(base('/swarm'))).role || 'none'; } catch { role = was ? was.role : 'none'; }
  known.set(env, { role, at: Date.now() });
  if (!was || was.role !== role) subs.forEach((fn) => fn());
  return role;
}

/** What a Swarm page shows on a host that is not a manager. */
export function notManager(role) {
  return emptyState(t('swarm.not_manager'), role === 'worker'
    ? t('swarm.worker')
    : t('swarm.no_swarm'));
}

/** A Swarm page: drawn by `page` once the host is known to be a swarm manager, else what it is instead. */
export function swarmPage(page) {
  return (ctx) => {
    const slot = h('div');
    ctx.el.append(slot);
    refreshSwarm().then((role) => {
      if (ctx.signal.aborted) return;
      if (role === 'manager') page({ ...ctx, el: slot });
      else mount(slot, notManager(role));
    });
  };
}

const TASK_TONES = { running: 'good', complete: 'good', new: 'info', pending: 'info', assigned: 'info', accepted: 'info', preparing: 'info', starting: 'warning', ready: 'info', shutdown: 'info', orphaned: 'warning', remove: 'info', failed: 'critical', rejected: 'critical' };
/** A task's state as FinUI's status badge, with what Docker said when it failed. */
export function taskBadge(task) {
  const s = (task.Status && task.Status.State) || 'unknown';
  return status({ tone: TASK_TONES[s] || 'info', line: true, class: 'nowrap' }, tOr(`swarm.task_state.${s}`, s));
}

/** A node's state and availability as badges: ready and active is good; drain and pause say so. */
export function nodeBadges(n) {
  const st = (n.Status && n.Status.State) || 'unknown', av = (n.Spec && n.Spec.Availability) || 'active';
  return h('span', { class: 'row' },
    status({ tone: st === 'ready' ? 'good' : st === 'down' ? 'critical' : 'warning', line: true, class: 'nowrap' }, tOr(`swarm.node_state.${st}`, st)),
    av !== 'active' ? status({ tone: 'warning', line: true, class: 'nowrap' }, tOr(`swarm.availability.${av}`, av)) : null);
}

/** A node's name as people say it: its host name. */
export const nodeName = (n) => (n && n.Description && n.Description.Hostname) || (n && n.ID ? n.ID.slice(0, 12) : '');

/** A service's mode and count: "replicated 2/3", "global 4/4", "job 1/1"; `bare` gives the count alone. */
export function replicas(s, { bare = false } = {}) {
  const m = (s.Spec && s.Spec.Mode) || {};
  const st = s.ServiceStatus || {};
  const run = st.RunningTasks ?? 0, want = st.DesiredTasks ?? (m.Replicated ? m.Replicated.Replicas : 0);
  const kind = m.Global ? 'global' : m.ReplicatedJob || m.GlobalJob ? 'job' : 'replicated';
  const tone = run === want ? 'good' : run === 0 ? 'critical' : 'warning';
  const badge = status({ tone, line: true }, `${run}/${want}`);
  return bare ? badge : h('span', { class: 'row nowrap' }, badge, h('span', { class: 'faint' }, t(`swarm.mode.${kind}`)));
}

export const serviceImage = (s) => String(((((s.Spec || {}).TaskTemplate || {}).ContainerSpec || {}).Image) || '').replace(/@sha256:[0-9a-f]+$/, '');
export const stackOf = (s) => ((s.Spec || {}).Labels || {})['com.docker.stack.namespace'] || null;

/** Published ports of a service: "8080:80/tcp". */
export function servicePorts(s) {
  const ports = ((s.Endpoint || {}).Ports) || (((s.Spec || {}).EndpointSpec || {}).Ports) || [];
  return ports.filter((p) => p.PublishedPort).map((p) => `${p.PublishedPort}:${p.TargetPort}${p.Protocol && p.Protocol !== 'tcp' ? '/' + p.Protocol : ''}`).join(', ');
}
