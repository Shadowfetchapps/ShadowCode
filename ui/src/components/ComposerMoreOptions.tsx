import {
  useEffect,
  useRef,
  useState,
  type MouseEvent,
  type ReactNode,
} from "react";
import { SlidersHorizontal } from "lucide-react";

/** Keeps infrequent task controls close at hand without crowding the composer. */
export function ComposerMoreOptions({
  children,
  indicator,
}: {
  children: ReactNode;
  indicator?: string;
}) {
  const root = useRef<HTMLDetailsElement>(null);
  const trigger = useRef<HTMLElement>(null);
  const [open, setOpen] = useState(false);

  useEffect(() => {
    if (!open) return;
    const onOutside = (event: PointerEvent) => {
      if (!root.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", onOutside);
    return () => document.removeEventListener("pointerdown", onOutside);
  }, [open]);

  const closeAfterAction = (event: MouseEvent<HTMLDivElement>) => {
    const button = (event.target as Element).closest("button");
    if (
      !button ||
      button.disabled ||
      button.getAttribute("aria-disabled") === "true"
    )
      return;
    const restoreFocus = Boolean(
      root.current?.contains(document.activeElement),
    );
    setOpen(false);
    if (restoreFocus)
      requestAnimationFrame(() => {
        if (root.current?.contains(document.activeElement))
          trigger.current?.focus();
      });
  };

  return (
    <details
      className="composer-more"
      ref={root}
      open={open}
      onKeyDown={(event) => {
        if (event.key === "Escape" && open) {
          event.preventDefault();
          event.stopPropagation();
          setOpen(false);
          trigger.current?.focus();
        }
      }}
    >
      <summary
        className="composer-more-trigger"
        aria-label={
          indicator
            ? `More task options, reasoning effort ${indicator}`
            : "More task options"
        }
        aria-expanded={open}
        title="Reasoning effort, parallel runs and other task options"
        ref={trigger}
        onClick={(event) => {
          event.preventDefault();
          setOpen((wasOpen) => !wasOpen);
        }}
      >
        <SlidersHorizontal size={14} aria-hidden="true" />
        <span>More</span>
        {indicator && (
          <span className="composer-more-indicator">{indicator}</span>
        )}
      </summary>
      <div className="composer-more-menu" onClick={closeAfterAction}>
        <div className="composer-more-heading">More task options</div>
        {children}
      </div>
    </details>
  );
}
