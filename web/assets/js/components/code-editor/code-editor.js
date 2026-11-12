// Freightdock: code-editor. A file to edit in the page (a Dockerfile, a compose file, a .env): the vendored CodeMirror 6
// (web/assets/vendor/codemirror, MIT, pre-built) with line numbers, undo, search, bracket matching and highlighting for
// YAML, Dockerfiles, .env and properties files and shell. Data-free in FinUI's sense: it is handed text and says when
// the text changes. Every colour is a FinUI token (code-editor.css): the highlighting gives classes, not colours, so a
// preset and both themes dress it.
//
// It is a plain text box first and becomes the editor once CodeMirror has loaded (one import for the whole page), so it
// can be typed into at once and still works if the editor cannot load. The API is the same either way.

import { h } from '../../../finui/core.js';
import { t } from '../../i18n.js';

let loading = null;
const load = () => (loading ||= import('/assets/vendor/codemirror/codemirror.js'));

/** The languages it knows, by the name callers give. `env` reads KEY=value lines as properties files do. */
const LANGUAGES = {
  text: () => [],
  yaml: (cm) => cm.yaml(),
  dockerfile: (cm) => cm.StreamLanguage.define(cm.dockerFile),
  env: (cm) => cm.StreamLanguage.define(cm.properties),
  properties: (cm) => cm.StreamLanguage.define(cm.properties),
  shell: (cm) => cm.StreamLanguage.define(cm.shell),
};

/** What each highlighted kind of text is called in CSS: `fd-code-editor__<kind>`, coloured by a token there. */
function highlight(cm) {
  const tags = cm.tags;
  const k = (kind, ...tags) => ({ tag: tags, class: `fd-code-editor__${kind}` });
  return cm.HighlightStyle.define([
    k('keyword', tags.keyword, tags.controlKeyword, tags.definitionKeyword, tags.moduleKeyword, tags.operatorKeyword),
    k('key', tags.propertyName, tags.definition(tags.propertyName), tags.attributeName, tags.definition(tags.variableName)),
    k('string', tags.string, tags.special(tags.string), tags.character, tags.content),
    k('number', tags.number, tags.integer, tags.float),
    k('atom', tags.bool, tags.null, tags.atom, tags.unit),
    k('variable', tags.variableName, tags.special(tags.variableName), tags.self),
    k('meta', tags.meta, tags.processingInstruction, tags.typeName, tags.labelName, tags.annotation, tags.tagName),
    k('comment', tags.comment, tags.lineComment, tags.blockComment, tags.docComment),
    k('punctuation', tags.punctuation, tags.separator, tags.operator, tags.bracket),
    k('invalid', tags.invalid),
  ]);
}

/**
 * codeEditor({ value, language, readOnly, onChange, label, rows, id }) → { el, getValue(), setValue(text),
 * setLanguage(name), setReadOnly(on), focus(), destroy() }.
 *
 * `language` is `yaml`, `dockerfile`, `env`, `properties`, `shell` or `text`. `onChange(text)` is told after each edit
 * the person makes (not after setValue). `rows` is how tall it starts; it grows with the text up to most of the screen.
 * `label` names it for a screen reader; put a visible label beside it (`id` is the editor's, for a label's `for`).
 */
export function codeEditor({ value = '', language = 'text', readOnly = false, onChange = () => {}, label = t('components.code_editor.label'), rows = 12, id = null } = {}) {
  if (!LANGUAGES[language]) throw new Error(`code-editor: no language "${language}"`);
  const el = h('div', { class: 'fd-code-editor', style: `--fd-code-editor-rows: ${rows}` });
  const area = h('textarea', { class: 'fui-field__input fui-field__input--area mono fd-code-editor__plain', id, rows, spellcheck: false,
    autocapitalize: 'none', autocomplete: 'off', 'aria-label': label, readOnly });
  area.value = value;
  area.addEventListener('input', () => onChange(area.value));
  el.append(area);
  let view = null, cm = null, lang = null, ro = null, quiet = false, gone = false;
  let want = { language, readOnly };

  load().then((mod) => {
    if (gone) return;
    cm = mod;
    lang = new cm.Compartment(); ro = new cm.Compartment();
    const editable = (on) => [cm.EditorState.readOnly.of(on), cm.EditorView.editable.of(!on)];
    const focused = document.activeElement === area;
    view = new cm.EditorView({
      parent: el,
      state: cm.EditorState.create({
        doc: area.value,
        extensions: [
          cm.lineNumbers(), cm.highlightActiveLineGutter(), cm.highlightSpecialChars(), cm.history(), cm.drawSelection(),
          cm.indentOnInput(), cm.bracketMatching(), cm.highlightActiveLine(), cm.highlightSelectionMatches(),
          cm.indentUnit.of('  '), cm.EditorState.tabSize.of(2),
          cm.syntaxHighlighting(highlight(cm)),
          cm.keymap.of([...cm.defaultKeymap, ...cm.historyKeymap, ...cm.searchKeymap, cm.indentWithTab]),
          cm.EditorView.contentAttributes.of({ 'aria-label': label, ...(id ? { id } : {}) }),
          lang.of(LANGUAGES[want.language](cm)),
          ro.of(editable(want.readOnly)),
          cm.EditorView.updateListener.of((u) => { if (u.docChanged && !quiet) onChange(u.state.doc.toString()); }),
        ],
      }),
    });
    view.setReadOnly = (on) => view.dispatch({ effects: ro.reconfigure(editable(on)) });
    area.remove();
    if (focused) view.focus();
  }).catch(() => { /* the text box stays: everything still works, only plainer */ });

  return {
    el,
    getValue: () => (view ? view.state.doc.toString() : area.value),
    setValue(text) {
      if (!view) { area.value = text; return; }
      quiet = true;
      try { view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: text } }); } finally { quiet = false; }
    },
    setLanguage(name) {
      if (!LANGUAGES[name]) throw new Error(`code-editor: no language "${name}"`);
      want.language = name;
      if (view) view.dispatch({ effects: lang.reconfigure(LANGUAGES[name](cm)) });
    },
    setReadOnly(on) { want.readOnly = !!on; area.readOnly = !!on; if (view) view.setReadOnly(!!on); },
    focus: () => (view ? view.focus() : area.focus()),
    destroy() { gone = true; if (view) view.destroy(); },
  };
}

export const meta = {
  name: 'code-editor',
  purpose: 'Edits a file in the page: a Dockerfile, a compose file, a .env, with line numbers, undo, search and highlighting.',
  use: 'Where a person writes a file that is read by a program: build a Dockerfile, a stack’s compose file and its .env.',
  avoid: 'A sentence or a short value (a field), or output to read (output-view, code-block).',
  a11y: 'A text box named by label. Tab indents; Escape then Tab leaves the editor. Until CodeMirror loads (or if it cannot) it is a plain text area with the same name.',
  props: {
    'codeEditor({ value, language, readOnly, onChange, label, rows, id })': 'language: yaml | dockerfile | env | properties | shell | text; onChange(text) after each edit by the person',
    'getValue() / setValue(text)': 'the text; setValue does not call onChange',
    'setLanguage(name) / setReadOnly(on)': 'change either while it is open',
  },
};
