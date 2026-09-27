import {
  memo,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ComponentPropsWithoutRef,
} from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Check, Copy } from "lucide-react";
import { parsedTreePlugin, type MarkdownTree } from "../lib/markdownTree";
import { markdownWorkerQueue } from "../lib/markdownWorker";

// Providers may send a response up to 16 MiB. Rendering all of that as one
// Markdown tree can make the desktop unresponsive, while the durable event and
// JSON export still retain the complete response.
export const MARKDOWN_PREVIEW_LIMIT = 512 * 1024;
export const MARKDOWN_WORKER_THRESHOLD = 8 * 1024;

/** Model/repo Markdown may contain javascript:, data:, file:, or relative
 * hrefs. Only absolute http(s) links and in-page fragments stay clickable. */
export function safeMarkdownHref(href?: string): string | undefined {
  if (!href) return undefined;
  if (href.startsWith("#") && !href.includes(":")) return href;
  try {
    const parsed = new URL(href);
    if (
      (parsed.protocol === "http:" || parsed.protocol === "https:") &&
      parsed.hostname &&
      !parsed.username &&
      !parsed.password
    ) {
      return href;
    }
  } catch {
    return undefined;
  }
  return undefined;
}

function CodeBlock({ children, ...props }: ComponentPropsWithoutRef<"pre">) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="code-block">
      <button
        type="button"
        className="copy-code"
        aria-label={copied ? "Code copied" : "Copy code"}
        onClick={async (e) => {
          const text =
            e.currentTarget.parentElement?.querySelector("pre")?.textContent ||
            "";
          try {
            await navigator.clipboard.writeText(text);
            setCopied(true);
            setTimeout(() => setCopied(false), 1800);
          } catch {
            setCopied(false);
          }
        }}
      >
        {copied ? <Check size={14} /> : <Copy size={14} />}
        {copied ? "Copied" : "Copy"}
      </button>
      <pre {...props}>{children}</pre>
    </div>
  );
}

const components = {
  pre: CodeBlock,
  a: ({ children, href, ...props }: ComponentPropsWithoutRef<"a">) => {
    const safe = safeMarkdownHref(href);
    if (!safe) return <span>{children}</span>;
    return (
      <a {...props} href={safe} target="_blank" rel="noopener noreferrer">
        {children}
      </a>
    );
  },
  img: ({ alt }: ComponentPropsWithoutRef<"img">) => (
    <span className="hint">[Image: {alt || "attachment"}]</span>
  ),
};
const ParsedMarkdown = memo(function ParsedMarkdown({
  tree,
}: {
  tree: MarkdownTree;
}) {
  const plugins = useMemo(() => parsedTreePlugin(tree), [tree]);
  return <ReactMarkdown rehypePlugins={plugins} components={components} />;
});

function LongMarkdown({ text }: { text: string }) {
  const current = useRef({ text, generation: 0 });
  if (text !== current.current.text) {
    // A finalized replacement or different answer invalidates old parses.
    // Appended stream prefixes can still display while the newest is pending.
    if (!text.startsWith(current.current.text)) current.current.generation++;
    current.current.text = text;
  }
  const generation = current.current.generation;
  const [result, setResult] = useState<{
    tree: MarkdownTree | null;
    generation: number;
    id: number;
  } | null>(null);
  const [failed, setFailed] = useState(false);
  const client = useRef<ReturnType<
    typeof markdownWorkerQueue.subscribe
  > | null>(null);
  useEffect(() => {
    client.current = markdownWorkerQueue.subscribe(
      (tree, parsedGeneration, id) => {
        if (parsedGeneration !== current.current.generation) return;
        setResult((previous) =>
          previous && previous.id >= id
            ? previous
            : { tree, generation: parsedGeneration, id },
        );
      },
      (reason) => {
        if (reason.kind === "unavailable") return setFailed(true);
        if (reason.generation !== current.current.generation) return;
        // A document that failed parsing must stay readable without retrying
        // the same failing parse on the UI thread. A newer snapshot can recover.
        setResult((previous) =>
          previous && previous.id >= reason.id
            ? previous
            : {
                tree: null,
                generation: reason.generation,
                id: reason.id,
              },
        );
      },
    );
    return () => {
      client.current?.dispose();
      client.current = null;
    };
  }, []);
  useEffect(() => {
    if (!failed) client.current?.update(text, generation);
  }, [text, generation, failed]);
  if (failed)
    return (
      <ReactMarkdown remarkPlugins={[remarkGfm]} components={components}>
        {text}
      </ReactMarkdown>
    );
  if (result?.generation === generation && result.tree)
    return <ParsedMarkdown tree={result.tree} />;
  // Retain readable source while the first parse is pending; no blank answer.
  return <div style={{ whiteSpace: "pre-wrap" }}>{text}</div>;
}

// Streaming one response must not reparse every earlier message.
export const Markdown = memo(function Markdown({
  children,
}: {
  children: string;
}) {
  const [expanded, setExpanded] = useState(false);
  const abbreviated = children.length > MARKDOWN_PREVIEW_LIMIT && !expanded;
  const visible = abbreviated
    ? children.slice(0, MARKDOWN_PREVIEW_LIMIT)
    : children;
  return (
    <div className="markdown">
      {visible.length >= MARKDOWN_WORKER_THRESHOLD &&
      typeof Worker !== "undefined" ? (
        <LongMarkdown text={visible} />
      ) : (
        <ReactMarkdown remarkPlugins={[remarkGfm]} components={components}>
          {visible}
        </ReactMarkdown>
      )}
      {abbreviated && (
        <div className="markdown-preview" role="status">
          <p className="hint">
            Showing the first 512 KiB of this response. The complete response is
            retained in task history and JSON export.
          </p>
          <button
            type="button"
            className="mini"
            onClick={() => setExpanded(true)}
          >
            Show full response
          </button>
        </div>
      )}
    </div>
  );
});
