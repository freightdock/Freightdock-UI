// The first run: the first administrator, with the setup token Freightdock printed to its log ("Sign-in" 14). Freightdock
// takes the local Docker as its first environment by itself.

import { post } from '../api.js';
import { state } from '../state.js';
import { navigate } from '../router.js';
import { authCard, credentialsForm } from './auth.js';
import { t } from '../i18n.js';

export function setupPage({ el }) {
  el.append(authCard(t('setup.title'), t('setup.sub'),
    credentialsForm({
      action: t('setup.action'), autocomplete: 'new-password',
      tokenHelp: t('setup.token_help'), passwordHelp: t('setup.password_help'),
      submit: async (c) => {
        await post('/api/setup', c);
        state.status = { ...state.status, configured: true };
        const { refreshMe } = await import('../app.js');
        await refreshMe();
        navigate('/', { replace: true });
      },
    })));
}
