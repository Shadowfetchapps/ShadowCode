import { parseMarkdownTree } from "../lib/markdownTree";

self.onmessage = (event: MessageEvent<{ id: number; text: string }>) => {
  const { id, text } = event.data;
  try {
    self.postMessage({ id, tree: parseMarkdownTree(text) });
  } catch (error) {
    self.postMessage({ id, error: String(error) });
  }
};
