import type { Extension } from "@codemirror/state";
import { StreamLanguage } from "@codemirror/language";

export type EditorLanguage =
  | "javascript"
  | "jsx"
  | "typescript"
  | "tsx"
  | "python"
  | "rust"
  | "json"
  | "html"
  | "css"
  | "markdown"
  | "yaml"
  | "shell"
  | "toml"
  | "go"
  | "c"
  | "cpp"
  | "java"
  | "dockerfile"
  | "plain";

/** Filename hints select highlighting only, never execution or file policy. */
export function editorLanguageForPath(path: string): EditorLanguage {
  const name = path.split(/[\\/]/).pop()?.toLowerCase() ?? "";
  if (name === "dockerfile" || name.startsWith("dockerfile."))
    return "dockerfile";
  if ([".bashrc", ".bash_profile", ".zshrc", ".profile"].includes(name))
    return "shell";
  const extension = name.includes(".") ? name.split(".").pop()! : "";
  const languages: Record<string, EditorLanguage> = {
    js: "javascript",
    mjs: "javascript",
    cjs: "javascript",
    jsx: "jsx",
    ts: "typescript",
    mts: "typescript",
    cts: "typescript",
    tsx: "tsx",
    py: "python",
    pyi: "python",
    rs: "rust",
    json: "json",
    html: "html",
    htm: "html",
    css: "css",
    md: "markdown",
    markdown: "markdown",
    yaml: "yaml",
    yml: "yaml",
    sh: "shell",
    bash: "shell",
    zsh: "shell",
    toml: "toml",
    go: "go",
    c: "c",
    h: "c",
    cc: "cpp",
    cpp: "cpp",
    cxx: "cpp",
    hpp: "cpp",
    hxx: "cpp",
    java: "java",
  };
  return Object.hasOwn(languages, extension) ? languages[extension] : "plain";
}

// Only language extensions are shared. Documents, selections and undo history
// belong to the mounted editor and are never retained in this module cache.
const loaded = new Map<EditorLanguage, Promise<Extension>>();

async function load(language: EditorLanguage): Promise<Extension> {
  switch (language) {
    case "javascript":
    case "jsx":
    case "typescript":
    case "tsx": {
      const { javascript } = await import("@codemirror/lang-javascript");
      return javascript({
        jsx: language === "jsx" || language === "tsx",
        typescript: language === "typescript" || language === "tsx",
      });
    }
    case "python":
      return (await import("@codemirror/lang-python")).python();
    case "rust":
      return (await import("@codemirror/lang-rust")).rust();
    case "json":
      return (await import("@codemirror/lang-json")).json();
    case "html":
      return (await import("@codemirror/lang-html")).html();
    case "css":
      return (await import("@codemirror/lang-css")).css();
    case "markdown":
      return (await import("@codemirror/lang-markdown")).markdown();
    case "yaml":
      return (await import("@codemirror/lang-yaml")).yaml();
    case "plain":
      return [];
    default: {
      switch (language) {
        case "shell":
          return StreamLanguage.define(
            (await import("@codemirror/legacy-modes/mode/shell")).shell,
          );
        case "toml":
          return StreamLanguage.define(
            (await import("@codemirror/legacy-modes/mode/toml")).toml,
          );
        case "go":
          return StreamLanguage.define(
            (await import("@codemirror/legacy-modes/mode/go")).go,
          );
        case "dockerfile":
          return StreamLanguage.define(
            (await import("@codemirror/legacy-modes/mode/dockerfile"))
              .dockerFile,
          );
        default:
          return StreamLanguage.define(
            (await import("@codemirror/legacy-modes/mode/clike"))[language],
          );
      }
    }
  }
}

export function loadEditorLanguage(path: string): Promise<Extension> {
  const language = editorLanguageForPath(path);
  let result = loaded.get(language);
  if (!result) {
    result = load(language).catch((error: unknown) => {
      // A missing chunk need not disable highlighting for the entire session.
      loaded.delete(language);
      throw error;
    });
    loaded.set(language, result);
  }
  return result;
}
