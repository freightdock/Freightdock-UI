// How Docker's things read in the UI: a container's name, its state as a tone, its ports as addresses, an image's name.

import { h, shortId } from './dom.js';
import { status } from '../finui/components/badge/badge.js';
import { state, currentEnv } from './state.js';
import { t, tOr } from './i18n.js';

export const env = () => state.env;
export const base = (p = '') => `/api/envs/${env()}${p}`;

export const containerName = (c) => String((c.Names && c.Names[0]) || c.Name || c.Id || '').replace(/^\//, '');

const TONES = { running: 'good', restarting: 'warning', paused: 'warning', created: 'info', exited: 'critical', dead: 'critical', removing: 'warning' };
/** A container's state as FinUI's status badge; a health check's verdict beside "running". */
export function stateBadge(state, health) {
  const s = String(state || 'unknown').toLowerCase();
  const tone = s === 'running' && health === 'unhealthy' ? 'critical' : s === 'running' && health === 'starting' ? 'warning' : TONES[s] || 'info';
  // Running and healthy says healthy: the health check is the stronger news, and one word keeps the column narrow.
  const words = s === 'running' && health && health !== 'none' ? health : s;
  return status({ tone, line: true, class: 'nowrap' }, tOr(`docker.state.${words}`, words));
}

/** "Up 3 hours (healthy)" says the health: Docker's list carries it only there. */
export const healthOf = (statusText) => (/\((healthy|unhealthy|health: starting)\)/.exec(statusText || '') || [])[1]?.replace('health: ', '') || null;

/** The host a published port is on: this server's for the local Docker, the agent's address, a TLS host's name. */
export function publicHost(e = currentEnv()) {
  if (e && e.kind === 'agent' && e.agent && e.agent.peer) return e.agent.peer;
  if (e && e.kind === 'tls') { const m = /^[a-z]+:\/\/\[?([^\]/]+?)\]?:\d+/.exec(e.url || ''); if (m) return m[1].includes(':') ? `[${m[1]}]` : m[1]; }
  return location.hostname;
}

/** Published ports, each a link to the host's port when it is TCP. */
export function portLinks(ports) {
  const seen = new Set();
  const out = [];
  for (const p of ports || []) {
    if (!p.PublicPort) continue;
    const key = `${p.PublicPort}/${p.PrivatePort}/${p.Type}`;
    if (seen.has(key)) continue;   // IPv4 and IPv6 say the same port twice
    seen.add(key);
    const text = `${p.PublicPort}:${p.PrivatePort}${p.Type === 'tcp' ? '' : '/' + p.Type}`;
    out.push(p.Type === 'tcp'
      ? h('a', { class: 'fui-chip mono', href: `${location.protocol === 'https:' ? 'https' : 'http'}://${publicHost()}:${p.PublicPort}`, target: '_blank', rel: 'noopener noreferrer' }, text)
      : h('span', { class: 'fui-chip mono' }, text));
  }
  return out.length ? h('span', { class: 'ports' }, out) : h('span', { class: 'faint' }, '');
}

/** An image's name as people say it: its first tag, else its short id. */
export function imageName(img) {
  const tags = (img.RepoTags || []).filter((tag) => tag !== '<none>:<none>');
  return tags[0] || (img.RepoDigests && img.RepoDigests[0] ? img.RepoDigests[0].split('@')[0] + ' ' + t('docker.untagged') : shortId(img.Id));
}

/** The compose project a container belongs to, if any. */
/** An environment's health as FinUI's status badge: up, down, or not heard from yet (an agent waiting to enrol). */
export function envBadge(e) {
  if (e.kind === 'agent' && e.agent && !e.agent.enrolled) return status({ tone: 'warning', line: true, class: 'nowrap' }, e.agent.join_expires_at ? t('docker.waiting_for_agent') : t('docker.not_enrolled'));
  const tone = { up: 'good', down: 'critical' }[e.status] || 'info';
  return status({ tone, line: true, class: 'nowrap' }, e.status === 'unknown' ? t('docker.not_checked_yet') : tOr(`docker.env_status.${e.status}`, e.status));
}

/** The update check's verdict as a badge, only when there is news: a newer image, or a registry that would not say. */
export function updateBadge(u) {
  if (!u) return null;
  if (u.status === 'outdated') return h('span', { title: u.detail || t('docker.newer_image') }, status({ tone: 'warning', line: true, class: 'nowrap' }, t('docker.update')));
  if (u.status === 'error') return h('span', { class: 'faint nowrap', title: u.detail || '' }, t('docker.not_checked'));
  return null;
}

export const projectOf = (c) => (c.Labels || c.Config?.Labels || {})['com.docker.compose.project'] || null;
