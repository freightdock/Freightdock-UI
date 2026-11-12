// The code editor the stack and template pages write compose files and variables in: the fd-code-editor component
// (CodeMirror 6, "UI" 4) in a labelled field, with help under it and an error that says what was wrong. Every page asks
// for an editor only through here, so the field around it is the same everywhere.

import { h } from './dom.js';
import { inlineError } from '../finui/components/field/field.js';
import { mount } from '../finui/core.js';
import { codeEditor as ssCodeEditor } from './components/code-editor/code-editor.js';
import { t } from './i18n.js';

/** What the variables box of a stack or template says under it. */
export const ENV_HELP = t('editor.env_help');

/** codeEditor({ id, label, value, rows, help, language, onChange }) → { el, focus(), value (get/set), setError(text) }.
 *  `language` is fd-code-editor's: `yaml` for a compose file, `env` for a .env; `text` when none is given. `onChange(text)`
 *  on every edit by hand. */
export function codeEditor({ id, label, value = '', rows = 18, help = null, language = null, onChange = () => {} }) {
  const editor = ssCodeEditor({ id, label, value, rows, language: language || 'text', onChange });
  const err = h('div');
  const el = h('div', { class: 'fui-field' },
    h('label', { class: 'fui-field__label', htmlFor: id }, label),
    editor.el,
    help ? h('p', { class: 'fui-field__help', id: `${id}-help` }, help) : null,
    err);
  return {
    el,
    focus: () => editor.focus(),
    get value() { return editor.getValue(); },
    set value(v) { editor.setValue(v); },
    setError(text) {
      editor.el.classList.toggle('fd-code-editor--invalid', !!text);
      mount(err, text ? inlineError(`${id}-err`, text) : '');
    },
  };
}
