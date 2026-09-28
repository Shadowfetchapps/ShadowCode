import { useLayoutEffect, useRef, useState } from "react";
import { basicSetup } from "codemirror";
import {
  Compartment,
  EditorState,
  Prec,
  type Extension,
} from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import { indentWithTab } from "@codemirror/commands";
import {
  HighlightStyle,
  indentUnit,
  syntaxHighlighting,
} from "@codemirror/language";
import { closeSearchPanel, searchPanelOpen } from "@codemirror/search";
import { tags } from "@lezer/highlight";
import { loadEditorLanguage } from "../lib/editorLanguage";

type LineSeparator = "\n" | "\r\n" | "\r";

/** null means genuinely mixed separators, requiring the parent's plain editor. */
function lineSeparator(value: string): LineSeparator | null {
  let separator: LineSeparator | undefined;
  for (const match of value.matchAll(/\r\n|\r|\n/g)) {
    const next = match[0] as LineSeparator;
    if (separator && separator !== next) return null;
    separator = next;
  }
  return separator ?? "\n";
}

export function hasMixedLineEndings(value: string): boolean {
  return lineSeparator(value) === null;
}

export type CodeEditorProps = {
  value: string;
  onChange: (value: string) => void;
  onSave: (value: string) => void;
  onEscape: () => void;
  disabled?: boolean;
  /** Keep open-file history while the parent displays its plain editor. */
  suspended?: boolean;
  workspace: string;
  path: string;
  openPaths: readonly string[];
};

type BufferState = {
  workspace: string;
  path: string;
  state: EditorState;
  language: Compartment;
  settings: Compartment;
  disabled: boolean;
  scrollTop: number;
  scrollLeft: number;
};

const highlighting = syntaxHighlighting(
  HighlightStyle.define([
    { tag: tags.keyword, color: "var(--editor-keyword)" },
    { tag: [tags.string, tags.regexp], color: "var(--editor-string)" },
    { tag: [tags.number, tags.bool, tags.atom], color: "var(--editor-number)" },
    { tag: tags.comment, color: "var(--editor-comment)" },
    {
      tag: [tags.typeName, tags.className, tags.namespace, tags.tagName],
      color: "var(--editor-type)",
    },
    { tag: tags.invalid, color: "var(--danger)" },
    { tag: tags.heading, fontWeight: "600" },
    { tag: tags.strong, fontWeight: "600" },
    { tag: tags.emphasis, fontStyle: "italic" },
    { tag: tags.link, textDecoration: "underline" },
  ]),
);

const editorTheme = EditorView.theme({
  "&": {
    height: "100%",
    color: "var(--text)",
    backgroundColor: "var(--surface)",
  },
  "&.cm-focused": { outline: "none" },
  ".cm-scroller": { overflow: "auto", fontFamily: "var(--mono)" },
  ".cm-content": { padding: "10px 0", caretColor: "var(--text)" },
  ".cm-line": { padding: "0 12px" },
  ".cm-gutters": {
    color: "var(--muted)",
    backgroundColor: "var(--surface-2)",
    borderRight: "1px solid var(--border)",
  },
  ".cm-activeLine, .cm-activeLineGutter": { backgroundColor: "var(--hover)" },
  ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--text)" },
  ".cm-selectionBackground, &.cm-focused .cm-selectionBackground": {
    backgroundColor: "var(--accent-soft)",
  },
  ".cm-searchMatch, .cm-searchMatch.cm-searchMatch-selected": {
    backgroundColor: "var(--accent-soft)",
    outline: "1px solid var(--accent)",
  },
  ".cm-matchingBracket": { backgroundColor: "var(--accent-soft)" },
  ".cm-panels, .cm-tooltip": {
    color: "var(--text)",
    backgroundColor: "var(--surface-2)",
    borderColor: "var(--border)",
  },
  ".cm-textfield, .cm-button": {
    color: "var(--text)",
    background: "var(--surface)",
    border: "1px solid var(--border-strong)",
  },
});

function settings(path: string, disabled: boolean): Extension {
  return [
    EditorState.readOnly.of(disabled),
    EditorView.editable.of(!disabled),
    EditorView.contentAttributes.of({
      "aria-label": `Edit ${path}`,
      "aria-readonly": String(disabled),
      "aria-disabled": String(disabled),
      spellcheck: "false",
      autocorrect: "off",
      autocapitalize: "off",
      tabindex: "0",
    }),
  ];
}

/** In-memory editor history lasts for this mounted editor, not across restarts.
 * Parent owns exact file bytes and durable draft/conflict/save policy. */
export function CodeEditor(props: CodeEditorProps) {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const buffers = useRef(new Map<string, BufferState>());
  const active = useRef<BufferState | null>(null);
  const callbacks = useRef(props);
  const languageRequest = useRef(0);
  const [languageUnavailable, setLanguageUnavailable] = useState(false);

  function createBuffer(): BufferState {
    const language = new Compartment();
    const configuration = new Compartment();
    let entry: BufferState;
    const state = EditorState.create({
      doc: props.value,
      extensions: [
        // basicSetup includes completion keys at this same highest priority.
        // Register our explicit save/exit policy first so Escape reaches it.
        Prec.highest(
          keymap.of([
            {
              key: "Mod-s",
              scope: "editor search-panel",
              preventDefault: true,
              stopPropagation: true,
              run: (editor) => {
                if (!callbacks.current.disabled)
                  callbacks.current.onSave(editor.state.sliceDoc());
                return true;
              },
            },
            {
              key: "Escape",
              scope: "editor search-panel",
              stopPropagation: true,
              run: (editor) => {
                if (searchPanelOpen(editor.state))
                  return closeSearchPanel(editor);
                callbacks.current.onEscape();
                return true;
              },
            },
            indentWithTab,
          ]),
        ),
        basicSetup,
        highlighting,
        editorTheme,
        indentUnit.of("  "),
        // sliceDoc uses this separator. Never serialize saved bytes with
        // Text.toString(), which joins lines with LF regardless of this facet.
        EditorState.lineSeparator.of(lineSeparator(props.value) ?? "\n"),
        configuration.of(settings(props.path, Boolean(props.disabled))),
        language.of([]),
        EditorView.clipboardInputFilter.of((text, state) =>
          text.replace(/\r\n|\r|\n/g, state.lineBreak),
        ),
        EditorView.updateListener.of((update) => {
          if (active.current !== entry) return;
          entry.state = update.state;
          if (update.docChanged)
            callbacks.current.onChange(update.state.sliceDoc());
        }),
      ],
    });
    entry = {
      workspace: props.workspace,
      path: props.path,
      state,
      language,
      settings: configuration,
      disabled: Boolean(props.disabled),
      scrollTop: 0,
      scrollLeft: 0,
    };
    return entry;
  }

  // Synchronize only committed props. An ordinary onChange echo does not
  // dispatch, reset selection or recreate the history extension.
  useLayoutEffect(() => {
    callbacks.current = props;
    if (!host.current) return;
    const open = new Set(props.openPaths);
    for (const [key, buffer] of buffers.current) {
      if (buffer.workspace !== props.workspace || !open.has(buffer.path))
        buffers.current.delete(key);
    }
    if (props.suspended) {
      // Mixed separators belong to the parent's byte-preserving plain editor.
      // Retire only the view: still-open ordinary files keep their own states,
      // and no mixed document enters CodeMirror's line-separator conversion.
      if (active.current && view.current) {
        active.current.scrollTop = view.current.scrollDOM.scrollTop;
        active.current.scrollLeft = view.current.scrollDOM.scrollLeft;
      }
      ++languageRequest.current;
      active.current = null;
      view.current?.destroy();
      view.current = null;
      setLanguageUnavailable(false);
      return;
    }
    const key = JSON.stringify([props.workspace, props.path]);
    let entry = buffers.current.get(key);
    if (!entry || entry.state.sliceDoc() !== props.value) {
      // A text-changing external replacement is authoritative. Starting a
      // fresh state prevents Undo from resurrecting the previous disk/draft.
      entry = createBuffer();
      buffers.current.set(key, entry);
    }
    if (active.current !== entry) {
      if (active.current && view.current) {
        active.current.scrollTop = view.current.scrollDOM.scrollTop;
        active.current.scrollLeft = view.current.scrollDOM.scrollLeft;
      }
      active.current = entry;
      if (view.current) view.current.setState(entry.state);
      else
        view.current = new EditorView({
          state: entry.state,
          parent: host.current,
        });
      view.current.scrollDOM.scrollTop = entry.scrollTop;
      view.current.scrollDOM.scrollLeft = entry.scrollLeft;
      setLanguageUnavailable(false);
      const request = ++languageRequest.current;
      const current = entry;
      void loadEditorLanguage(current.path).then(
        (extension) => {
          if (
            request !== languageRequest.current ||
            active.current !== current ||
            !view.current
          )
            return;
          view.current.dispatch({
            effects: current.language.reconfigure(extension),
          });
        },
        () => {
          if (
            request === languageRequest.current &&
            active.current === current &&
            view.current
          )
            setLanguageUnavailable(true);
        },
      );
    }
    const disabled = Boolean(props.disabled);
    if (entry.disabled !== disabled) {
      entry.disabled = disabled;
      view.current?.dispatch({
        effects: entry.settings.reconfigure(settings(entry.path, disabled)),
      });
    }
  });

  useLayoutEffect(
    () => () => {
      ++languageRequest.current;
      active.current = null;
      view.current?.destroy();
      view.current = null;
      buffers.current.clear();
    },
    [],
  );

  return (
    <div
      className="code-editor"
      data-disabled={Boolean(props.disabled)}
      hidden={props.suspended}
    >
      <div className="code-editor-host" ref={host} />
      {languageUnavailable && (
        <p className="code-editor-status" role="status">
          Syntax highlighting is unavailable. Editing and saving still work.
        </p>
      )}
    </div>
  );
}
