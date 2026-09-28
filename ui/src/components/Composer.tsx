import {
  useEffect,
  useRef,
  useState,
  type ReactNode,
  type RefObject,
} from "react";
import {
  ArrowUp,
  Eye,
  FileCode2,
  Folder,
  ListPlus,
  LoaderCircle,
  Paperclip,
  Square,
  X,
} from "lucide-react";
import { useAgentOptions } from "./AgentMentions";
import {
  MentionMenu,
  useFileMentions,
  type MentionOption,
} from "./MentionMenu";
import {
  insertMention,
  mentionAt,
  mentionToken,
  removeMentionText,
  type Mention,
} from "../lib/mentions";
import type { PromptHistory } from "../hooks/useComposerExtras";
import {
  IMAGE_ACCEPT,
  TEXT_ACCEPT,
  pastedImages,
  type Attachment,
} from "../lib/attachments";
import type { ContextAttachment } from "../lib/pendingAttachments";
import { ContextChips } from "./ContextChips";
import { ContextInventory } from "./ContextInventory";

export type SlashCommand = {
  name: string;
  description: string;
  arg_spec: string;
};

/** Message field, attachments, the model picker and Send/Stop. Everything that
 * decides whether a message may be sent is computed by the caller. */
export function Composer({
  task,
  onTask,
  promptRef,
  attachments,
  onRemoveAttachment,
  onAttach,
  canAttachImages,
  attachDisabled,
  picker,
  controls,
  commands,
  placeholder,
  hint,
  busy,
  queueing,
  submitting,
  locked,
  canSend,
  sendBlocked,
  stopDisabled,
  onSubmit,
  onSubmitWorktree,
  onStop,
  more,
  voice,
  mentions = [],
  onMention,
  onRemoveMention,
  context = [],
  onRemoveContext,
  history,
}: {
  task: string;
  onTask: (value: string) => void;
  promptRef: RefObject<HTMLTextAreaElement | null>;
  attachments: Attachment[];
  onRemoveAttachment: (path: string) => void;
  onAttach: (files: File[]) => void;
  canAttachImages: boolean;
  attachDisabled: boolean;
  picker: ReactNode;
  controls: ReactNode;
  commands: SlashCommand[];
  placeholder: string;
  hint: string;
  busy: boolean;
  queueing: boolean;
  submitting: boolean;
  locked: boolean;
  canSend: boolean;
  /** Why Send is unavailable (shown under the field). */
  sendBlocked: string | null;
  stopDisabled: boolean;
  onSubmit: () => void;
  /** Ctrl+Shift+Enter: run in a new worktree (when possible now). */
  onSubmitWorktree?: () => void;
  onStop: () => void;
  /** Optional, less-frequent controls in a compact disclosure. */
  more?: ReactNode;
  /** The dictation mic button, next to Attach. */
  voice?: ReactNode;
  /** Files and folders picked from the @ menu (chips). Without
   * `onMention` the @ menu offers subagents only. */
  mentions?: Mention[];
  onMention?: (mention: Mention) => void;
  onRemoveMention?: (path: string) => void;
  /** Elements and console messages from the Preview tab (chips). */
  context?: readonly ContextAttachment[];
  onRemoveContext?: (id: string) => void;
  /** ↑/↓ recall of earlier prompts while the field is empty. */
  history?: PromptHistory;
}) {
  const fileRef = useRef<HTMLInputElement>(null);
  const [slashIndex, setSlashIndex] = useState(0);
  const [slashOpen, setSlashOpen] = useState(false);
  const [dragging, setDragging] = useState(false);
  const [contextInventoryOpen, setContextInventoryOpen] = useState(false);
  const hits = slashOpen
    ? commands.filter((c) => c.name.startsWith(task.slice(1))).slice(0, 8)
    : [];
  // The @ menu: `@name` at the start of a message names a subagent;
  // `@path` anywhere attaches a project file or folder.
  const [caret, setCaret] = useState(task.length);
  const [mentionIndex, setMentionIndex] = useState(0);
  const [closedAt, setClosedAt] = useState<number | null>(null);
  const token = mentionAt(task, caret);
  const menuToken = token && token.start !== closedAt ? token : null;
  const agentQuery =
    menuToken && menuToken.start === 0 && /^[\w-]*$/.test(menuToken.query)
      ? menuToken.query
      : null;
  const agents = useAgentOptions(agentQuery !== null);
  const files = useFileMentions(
    menuToken && onMention ? menuToken.query : null,
  );
  const options: MentionOption[] = menuToken
    ? [
        ...agents
          .filter((a) => agentQuery !== null && a.name.startsWith(agentQuery))
          .slice(0, 6)
          .map((agent) => ({ kind: "agent" as const, agent })),
        ...files.items
          .filter((f) => !mentions.some((m) => m.path === f.path))
          .map((f) => ({ kind: f.kind, path: f.path })),
      ]
    : [];
  const menuOpen = Boolean(
    menuToken && (options.length || (onMention && menuToken.query)),
  );
  const activeMention = Math.min(mentionIndex, Math.max(0, options.length - 1));
  const moveCaret = (at: number) => {
    setCaret(at);
    requestAnimationFrame(() => {
      const el = promptRef.current;
      if (el) {
        el.focus();
        el.setSelectionRange(at, at);
      }
    });
  };
  const pickMention = (option: MentionOption) => {
    if (!menuToken) return;
    const value =
      option.kind === "agent"
        ? `@${option.agent.name}`
        : mentionToken({ path: option.path, kind: option.kind });
    const next = insertMention(task, menuToken, value);
    onTask(next.text);
    if (option.kind !== "agent")
      onMention?.({ path: option.path, kind: option.kind });
    setClosedAt(menuToken.start);
    setMentionIndex(0);
    moveCaret(next.caret);
  };
  const trackCaret = (el: HTMLTextAreaElement) =>
    setCaret(el.selectionStart ?? el.value.length);
  useEffect(() => {
    const el = promptRef.current;
    if (el) {
      el.style.height = "auto";
      el.style.height = `${Math.min(el.scrollHeight, 200)}px`;
    }
  }, [task, promptRef]);
  useEffect(() => {
    if (!task.startsWith("/")) setSlashOpen(false);
  }, [task]);
  return (
    <div
      className={`composer-shell ${dragging ? "is-dragging" : ""}`}
      onDragOver={(e) => {
        if (e.dataTransfer.types.includes("Files")) {
          e.preventDefault();
          setDragging(true);
        }
      }}
      onDragLeave={() => setDragging(false)}
      onDrop={(e) => {
        e.preventDefault();
        setDragging(false);
        if (e.dataTransfer.files.length)
          onAttach(Array.from(e.dataTransfer.files));
      }}
    >
      {menuOpen && (
        <MentionMenu
          options={options}
          index={activeMention}
          loading={files.loading}
          onPick={pickMention}
        />
      )}
      {slashOpen && (
        <div className="slash-menu" role="listbox" aria-label="Slash commands">
          {hits.length ? (
            hits.map((c, i) => (
              <div
                role="option"
                aria-selected={i === slashIndex}
                className={`slash-hit ${i === slashIndex ? "on" : ""}`}
                key={c.name}
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => {
                  onTask(`/${c.name}${c.arg_spec ? " " : ""}`);
                  setSlashOpen(false);
                  promptRef.current?.focus();
                }}
              >
                <strong>/{c.name}</strong>
                <span>{c.description}</span>
              </div>
            ))
          ) : (
            <div className="slash-empty">No matching commands</div>
          )}
        </div>
      )}
      <form
        className={`composer ${locked ? "is-working" : ""}`}
        onSubmit={(e) => {
          e.preventDefault();
          onSubmit();
        }}
      >
        {(attachments.length > 0 || context.length > 0) && (
          <ul className="chips" aria-label="Attachments">
            {attachments.map((a) => (
              <li key={a.path} className="path-chip">
                {a.preview ? (
                  <img src={a.preview} alt="" className="chip-preview" />
                ) : (
                  <FileCode2 size={12} aria-hidden="true" />
                )}
                <span>{a.name}</span>
                <button
                  type="button"
                  className="chip-remove"
                  aria-label={`Remove ${a.name}`}
                  onClick={() => onRemoveAttachment(a.path)}
                >
                  <X size={12} aria-hidden="true" />
                </button>
              </li>
            ))}
            <ContextChips items={context} onRemove={onRemoveContext} />
          </ul>
        )}
        {mentions.length > 0 && (
          <>
            <ul className="chips" aria-label="Mentioned files and folders">
              {mentions.map((m) => (
                <li key={m.path} className="path-chip mention-chip">
                  {m.kind === "dir" ? (
                    <Folder size={12} aria-hidden="true" />
                  ) : (
                    <FileCode2 size={12} aria-hidden="true" />
                  )}
                  <span title={m.path}>{mentionToken(m).slice(1)}</span>
                  <button
                    type="button"
                    className="chip-remove"
                    aria-label={`Remove ${m.path}`}
                    onClick={() => {
                      onTask(removeMentionText(task, m));
                      onRemoveMention?.(m.path);
                      promptRef.current?.focus();
                    }}
                  >
                    <X size={12} aria-hidden="true" />
                  </button>
                </li>
              ))}
            </ul>
            <button
              type="button"
              className="context-inventory-trigger"
              aria-expanded={contextInventoryOpen}
              onClick={() => setContextInventoryOpen((open) => !open)}
            >
              <Eye size={13} aria-hidden="true" />
              Inspect attached context
            </button>
            {contextInventoryOpen && (
              <ContextInventory
                mentions={mentions}
                onClose={() => setContextInventoryOpen(false)}
              />
            )}
          </>
        )}
        <textarea
          ref={promptRef}
          aria-autocomplete={onMention ? "list" : undefined}
          aria-activedescendant={
            menuOpen && options.length ? `mention-${activeMention}` : undefined
          }
          aria-label="Message ShadowCode"
          aria-describedby={sendBlocked ? "composer-blocked" : undefined}
          value={task}
          rows={2}
          placeholder={placeholder}
          onChange={(e) => {
            onTask(e.target.value);
            trackCaret(e.target);
            history?.reset();
            setSlashIndex(0);
            setMentionIndex(0);
            setClosedAt(null);
            setSlashOpen(/^\/\S*$/.test(e.target.value));
          }}
          onSelect={(e) => trackCaret(e.currentTarget)}
          onPaste={(e) => {
            const images = pastedImages(e.clipboardData);
            if (images.length) {
              e.preventDefault();
              onAttach(images);
            }
          }}
          onKeyDown={(e) => {
            if (menuOpen) {
              const count = options.length;
              if ((e.key === "ArrowDown" || e.key === "ArrowUp") && count) {
                e.preventDefault();
                setMentionIndex(
                  (i) =>
                    (Math.min(i, count - 1) +
                      (e.key === "ArrowDown" ? 1 : count - 1)) %
                    count,
                );
                return;
              }
              if ((e.key === "Tab" || e.key === "Enter") && count) {
                e.preventDefault();
                pickMention(options[activeMention]);
                return;
              }
              if (e.key === "Escape") {
                e.stopPropagation();
                if (menuToken) setClosedAt(menuToken.start);
                return;
              }
            }
            if (slashOpen && hits.length) {
              if (e.key === "ArrowDown" || e.key === "ArrowUp") {
                e.preventDefault();
                setSlashIndex(
                  (i) =>
                    (i + (e.key === "ArrowDown" ? 1 : hits.length - 1)) %
                    hits.length,
                );
                return;
              }
              if (e.key === "Tab") {
                e.preventDefault();
                onTask(`/${hits[slashIndex].name} `);
                setSlashOpen(false);
                return;
              }
            }
            if (
              history &&
              !slashOpen &&
              !e.shiftKey &&
              !e.altKey &&
              !e.metaKey &&
              !e.ctrlKey &&
              (e.key === "ArrowUp" || e.key === "ArrowDown")
            ) {
              const el = e.currentTarget;
              const firstLine = !el.value
                .slice(0, el.selectionStart ?? 0)
                .includes("\n");
              const lastLine = !el.value
                .slice(el.selectionEnd ?? el.value.length)
                .includes("\n");
              const text =
                e.key === "ArrowUp" &&
                firstLine &&
                (!task || history.browsing())
                  ? history.older(task)
                  : e.key === "ArrowDown" && lastLine && history.browsing()
                    ? history.newer()
                    : null;
              if (text !== null) {
                e.preventDefault();
                onTask(text);
                moveCaret(text.length);
                return;
              }
            }
            if (e.key === "Escape" && slashOpen) {
              e.stopPropagation();
              setSlashOpen(false);
              return;
            }
            if (
              e.key === "Enter" &&
              e.shiftKey &&
              (e.ctrlKey || e.metaKey) &&
              onSubmitWorktree &&
              !e.nativeEvent.isComposing
            ) {
              e.preventDefault();
              onSubmitWorktree();
              return;
            }
            if (
              e.key === "Enter" &&
              !e.shiftKey &&
              !e.nativeEvent.isComposing
            ) {
              e.preventDefault();
              onSubmit();
            }
          }}
        />
        <div className="composer-footer">
          <button
            type="button"
            className="icon-btn attach-btn"
            aria-label={
              canAttachImages ? "Attach files or images" : "Attach text files"
            }
            title={
              canAttachImages
                ? "Attach files or images (PNG, JPEG, WebP). You can also paste or drop images."
                : "Attach text files. Choose a model marked Vision to attach images."
            }
            disabled={attachDisabled}
            onClick={() => fileRef.current?.click()}
          >
            <Paperclip size={17} aria-hidden="true" />
          </button>
          <input
            ref={fileRef}
            type="file"
            multiple
            accept={
              canAttachImages ? `${IMAGE_ACCEPT},${TEXT_ACCEPT}` : TEXT_ACCEPT
            }
            hidden
            onChange={(e) => {
              if (e.target.files) onAttach(Array.from(e.target.files));
              e.target.value = "";
            }}
          />
          {voice}
          {picker}
          {controls}
          <span className="grow" />
          <span className="composer-hint">{hint}</span>
          {more}
          {busy && (
            <button
              type="button"
              className="submit-btn stop"
              aria-label="Stop task"
              title="Stop task (Ctrl+.)"
              disabled={stopDisabled}
              onClick={onStop}
            >
              <Square size={14} fill="currentColor" aria-hidden="true" />
            </button>
          )}
          <button
            type="submit"
            className="submit-btn"
            aria-label={queueing ? "Queue follow-up" : "Send task"}
            title={sendBlocked || (queueing ? "Queue follow-up" : "Send task")}
            disabled={!canSend}
          >
            {submitting ? (
              <LoaderCircle size={17} className="spin" aria-hidden="true" />
            ) : queueing ? (
              <ListPlus size={19} aria-hidden="true" />
            ) : (
              <ArrowUp size={19} aria-hidden="true" />
            )}
          </button>
        </div>
      </form>
      {sendBlocked && (
        <p className="composer-blocked" id="composer-blocked" role="status">
          {sendBlocked}
        </p>
      )}
    </div>
  );
}
