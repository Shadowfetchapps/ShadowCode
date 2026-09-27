import ReactMarkdown, { type Options } from "react-markdown";
import remarkGfm from "remark-gfm";
import type { Root } from "hast";

export type MarkdownTree = Root;

/** Use the renderer's own complete-document pipeline, including reference
 * resolution and GFM. Capturing at its public rehype boundary avoids a second
 * parser or splitting constructs across independently parsed text chunks.
 * The empty replacement avoids constructing unused React nodes in the worker.
 */
export function parseMarkdownTree(text: string): MarkdownTree {
  let parsed: MarkdownTree | undefined;
  ReactMarkdown({
    children: text,
    remarkPlugins: [remarkGfm],
    rehypePlugins: [
      () => (tree: Root) => {
        parsed = tree;
        return { type: "root", children: [] } as Root;
      },
    ],
  });
  if (!parsed) throw new Error("Markdown parser did not return a document");
  return parsed;
}

/** ReactMarkdown still owns raw-HTML escaping, URL sanitization and React
 * rendering on the main thread, with the same application components.
 */
export function parsedTreePlugin(tree: MarkdownTree): Options["rehypePlugins"] {
  return [() => () => tree];
}
