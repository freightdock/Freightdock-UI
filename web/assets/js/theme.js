// The theme this browser chose, applied before the first paint: a classic script in <head>, so it blocks, where a module
// would run after the page had been painted in the device's colours. Nothing stored means "follow the device", which
// FinUI's tokens do by themselves. state.js reads and writes the same key.
try { const theme = localStorage.getItem('freightdock.theme'); if (theme === 'light' || theme === 'dark') document.documentElement.dataset.theme = theme; } catch { /* storage blocked: the device decides */ }
// The words of the language this browser reads Freightdock in (i18n.js decides the same way, and LANGUAGES there is this
// list): fetched while the rest of the page loads, so main.js finds them already here. <html lang> is set now, for the
// first paint's hyphenation and screen readers.
try {
  const tags = ['en', 'nb'];
  const of = (raw) => { const l = String(raw || '').toLowerCase().split('-')[0]; return l === 'no' || l === 'nn' ? 'nb' : tags.includes(l) ? l : null; };
  let tag = null;
  try { tag = of(localStorage.getItem('freightdock.language')); } catch { /* storage blocked: the browser's languages decide */ }
  if (!tag) for (const raw of navigator.languages || [navigator.language]) { tag = of(raw); if (tag) break; }
  tag = tag || 'en';
  document.documentElement.lang = tag;
  document.write('<link rel="preload" href="/assets/i18n/' + tag + '.json" as="fetch" crossorigin="anonymous">');
} catch { /* i18n.js decides by itself */ }
