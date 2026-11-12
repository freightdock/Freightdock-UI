// Boot: the words of the language this browser reads Freightdock in first (i18n.js, "UI" 17), then the app, so a module
// may say t() at its top level.

import { loadLanguage } from './i18n.js';

await loadLanguage();
await import('./app.js');
