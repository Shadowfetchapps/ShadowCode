import { useId, type ReactNode } from "react";
import { GLOSSARY, type GlossaryWord } from "../lib/glossary";

/** A word with its plain meaning on hover or keyboard focus. */
export function Term({
  word,
  children,
}: {
  word: GlossaryWord;
  children?: ReactNode;
}) {
  const id = useId();
  return (
    <span className="term" tabIndex={0} aria-describedby={id}>
      {children ?? word}
      <span role="tooltip" id={id} className="term-tip">
        {GLOSSARY[word]}
      </span>
    </span>
  );
}
