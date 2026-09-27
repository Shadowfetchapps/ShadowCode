import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { parseMarkdownTree, parsedTreePlugin } from "./markdownTree";

const documents = [
  '[A reference][later]\n\nParagraph between the reference and definition.\n\n[later]: https://example.com "Reference title"',
  "- First paragraph\n\n  Continuation of the same list item.\n\n  ```ts\n  const n = 1;\n\n  console.log(n);\n  ```\n\n- Second item\n",
  "| Left | Right |\n| :--- | ---: |\n| **strong** | `code` |\n\n- [x] Done\n- [ ] Pending\n\n~~removed~~ and https://example.com\n",
  "Footnote here[^note].\n\nAnother paragraph.\n\n[^note]: First footnote paragraph.\n\n    Second paragraph.\n",
  "<script>alert('unsafe')</script>\n\n<img src=x onerror=alert(1)>\n\n[unsafe](javascript:alert(1))\n\n![remote](https://example.com/image.png)",
  "Heading\n=======\n\n> First quote paragraph.\n>\n> Second quote paragraph.\n\n```js\nconst unfinished = true;\n\n",
  "# Unicode 😀\n\n```text\nline one\n\nline three\n```\n\nEscaped \\*stars\\* & ampersands.",
];

describe("complete-document worker Markdown pipeline", () => {
  for (const [index, document] of documents.entries()) {
    it(`matches the existing renderer for semantic fixture ${index + 1}`, () => {
      const expected = renderToStaticMarkup(
        <ReactMarkdown remarkPlugins={[remarkGfm]}>{document}</ReactMarkdown>,
      );
      // Structured cloning is the worker boundary; no React nodes or live
      // parser objects may be required on the rendering side.
      const tree = structuredClone(parseMarkdownTree(document));
      const actual = renderToStaticMarkup(
        <ReactMarkdown rehypePlugins={parsedTreePlugin(tree)} />,
      );
      expect(actual).toBe(expected);
    });
  }
  it("resolves a later definition and closes an earlier fence after appended output", () => {
    const prefix = "[reference][later]\n\n```ts\nconst n = 1;\n\n";
    const completed = `${prefix}console.log(n);\n\`\`\`\n\n[later]: https://example.com\n`;
    const expected = renderToStaticMarkup(
      <ReactMarkdown remarkPlugins={[remarkGfm]}>{completed}</ReactMarkdown>,
    );
    const actual = renderToStaticMarkup(
      <ReactMarkdown
        rehypePlugins={parsedTreePlugin(parseMarkdownTree(completed))}
      />,
    );
    expect(actual).toBe(expected);
    expect(actual).toContain('href="https://example.com"');
    expect(actual).toContain("const n = 1;\n\nconsole.log(n);\n");
  });
});
