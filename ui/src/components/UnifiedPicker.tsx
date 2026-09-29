import {
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
} from "react";
import {
  Check,
  ChevronDown,
  ChevronRight,
  Eye,
  Info,
  KeyRound,
  MessageSquareText,
  Plus,
  RefreshCw,
  Search,
} from "lucide-react";
import {
  availabilityLabel,
  billingWarning,
  groupTargets,
  isApiKey,
  isLocal,
  isReady,
  matchesQuery,
  recentTargets,
  rowAction,
  shortName,
  usageAfterAvailability,
  usageDetailLines,
  usageLabel,
  vendorKey,
  vendorLabel,
  vendorSections,
  type PickerTarget,
} from "../lib/picker";

type Item =
  | { kind: "row"; key: string; target: PickerTarget }
  | {
      kind: "more";
      key: string;
      vendor: string;
      label: string;
      hidden: number;
    }
  | { kind: "add-key"; key: string; vendor: string; label: string };

type Group = {
  id: string;
  title: string;
  note?: string;
  items: Item[];
  empty: string;
};

/** The vendor whose API key the empty API-keys group offers to add. */
const API_VENDOR = "openrouter";

/** The one model control: subscription rows, API-key rows (OpenRouter) and
 * local GGUF rows from GET /api/picker in a searchable listbox (combobox
 * pattern). */
export function UnifiedPicker({
  targets,
  value,
  open,
  onOpenChange,
  onSelect,
  onConnect,
  onSetup,
  onAddLocal,
  onRefresh,
  note,
  loading,
  label = "Model for this task",
}: {
  targets: PickerTarget[];
  value: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSelect: (id: string) => void;
  /** Accounts › Connect, optionally for one vendor ("codex", "openrouter"). */
  onConnect: (vendor?: string) => void;
  /** Where a setup-required row is fixed (Accounts or Local models). */
  onSetup: (target: PickerTarget) => void;
  onAddLocal: () => void;
  onRefresh?: () => Promise<void>;
  note?: string;
  loading?: boolean;
  /** Accessible name of the trigger, before the chosen row's name. */
  label?: string;
}) {
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const [expanded, setExpanded] = useState<string[]>([]);
  const [details, setDetails] = useState<string | null>(null);
  const [recent] = useState(recentTargets);
  const [refreshing, setRefreshing] = useState(false);
  const trigger = useRef<HTMLButtonElement>(null);
  const root = useRef<HTMLDivElement>(null);
  const search = useRef<HTMLInputElement>(null);
  const uid = useId().replace(/:/g, "");
  const listId = `${uid}-list`;
  const selected = targets.find((t) => t.id === value);

  const groups = useMemo<Group[]>(() => {
    const matching = targets.filter((t) => matchesQuery(t, query));
    const all = groupTargets(matching);
    const build = (rows: PickerTarget[]): Item[] =>
      vendorSections(rows, {
        expanded,
        recent,
        selected: value,
        searching: Boolean(query.trim()),
      }).flatMap((section) => [
        ...section.rows.map((target): Item => ({
          kind: "row",
          key: target.id,
          target,
        })),
        ...(section.hidden
          ? [
              {
                kind: "more" as const,
                key: `more:${section.key}`,
                vendor: section.key,
                label: section.label,
                hidden: section.hidden,
              },
            ]
          : []),
      ]);
    // With no API-key rows at all, the group offers to add a key instead.
    const noApiRows = !targets.some(isApiKey);
    const mixedVendorBilling = all.subscriptions.some(
      (target) => target.billing === "unknown" || target.billing === "api_key",
    );
    const subscriptions = {
      id: "subscriptions",
      title: mixedVendorBilling ? "Vendor CLIs" : "Subscriptions",
      items: build(all.subscriptions),
      empty: query
        ? mixedVendorBilling
          ? "No vendor CLI matches"
          : "No subscription matches"
        : "No accounts connected yet",
    };
    // With no account connected yet (a first run), the free models on this
    // computer come first.
    // (Judged on every row, so the order doesn't change while searching.)
    const beginner = !groupTargets(targets).subscriptions.some(isReady);
    // Free local models come before paid API rows, so a search that
    // matches both picks the local one on Enter.
    const local = {
      id: "local",
      title: "On this computer",
      ...(beginner ? { note: "Free · your code stays on this computer" } : {}),
      items: build(all.local),
      empty: query ? "No local model matches" : "No local models added yet",
    };
    const api = {
      id: "api",
      title: "API keys",
      note: "Billed per token by the provider",
      items:
        noApiRows && !query.trim()
          ? [
              {
                kind: "add-key" as const,
                key: `add-key:${API_VENDOR}`,
                vendor: API_VENDOR,
                label: "Add an OpenRouter API key…",
              },
            ]
          : build(all.api),
      empty: "No API-key model matches",
    };
    return beginner ? [local, subscriptions, api] : [subscriptions, local, api];
  }, [targets, query, expanded, recent, value]);
  const items = useMemo(() => groups.flatMap((g) => g.items), [groups]);
  const optionId = (item: Item) =>
    `${uid}-opt-${item.key.replace(/[^a-zA-Z0-9_-]/g, "_")}`;
  const activeItem = items[Math.min(active, items.length - 1)];
  const detailTarget = details
    ? targets.find((t) => t.id === details)
    : undefined;

  useEffect(() => {
    if (!open) return;
    const index = items.findIndex(
      (item) => item.kind === "row" && item.target.id === value,
    );
    setActive(index >= 0 ? index : 0);
    setDetails(null);
    // Focus the search field once the menu is in the document.
    requestAnimationFrame(() => search.current?.focus());
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const onDoc = (event: MouseEvent) => {
      if (!root.current?.contains(event.target as Node)) close(false);
    };
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  });

  useEffect(() => {
    if (!open || !activeItem) return;
    document
      .getElementById(optionId(activeItem))
      ?.scrollIntoView?.({ block: "nearest" });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active, open]);

  function close(restoreFocus = true) {
    onOpenChange(false);
    setQuery("");
    setDetails(null);
    if (restoreFocus) requestAnimationFrame(() => trigger.current?.focus());
  }

  function activate(item: Item | undefined) {
    if (!item) return;
    if (item.kind === "more") {
      setExpanded((keys) => [...keys, item.vendor]);
      return;
    }
    if (item.kind === "add-key") {
      close(false);
      onConnect(item.vendor);
      return;
    }
    const target = item.target;
    const action = rowAction(target);
    if (action.kind === "select") {
      onSelect(target.id);
      close();
    } else if (action.kind === "connect") {
      close(false);
      onConnect(action.vendor);
    } else if (details === target.id && action.kind === "setup") {
      close(false);
      onSetup(target);
    } else {
      setDetails(target.id);
    }
  }

  function onKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    const last = items.length - 1;
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        setActive((i) => Math.min(last, i + 1));
        break;
      case "ArrowUp":
        event.preventDefault();
        setActive((i) => Math.max(0, i - 1));
        break;
      case "Home":
        event.preventDefault();
        setActive(0);
        break;
      case "End":
        event.preventDefault();
        setActive(Math.max(0, last));
        break;
      case "Enter":
        event.preventDefault();
        activate(activeItem);
        break;
      case "ArrowRight":
        if (activeItem?.kind === "row" && !query) {
          event.preventDefault();
          setDetails(activeItem.target.id);
        }
        break;
      case "ArrowLeft":
        if (details && !query) {
          event.preventDefault();
          setDetails(null);
        }
        break;
      case "Escape":
        event.preventDefault();
        event.stopPropagation();
        if (details) setDetails(null);
        else close();
        break;
    }
  }

  // Keep the detail pane on the row the keyboard is on once it is open.
  useEffect(() => {
    if (
      details &&
      activeItem?.kind === "row" &&
      activeItem.target.id !== details
    )
      setDetails(activeItem.target.id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active]);

  return (
    <div className="unified-picker" ref={root}>
      <button
        ref={trigger}
        type="button"
        className={`unified-picker-trigger ${selected ? "" : "is-empty"}`}
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label={
          selected
            ? `${label}: ${selected.name}${billingWarning(selected) ? `; ${billingWarning(selected)}` : ""}`
            : `${label}: none chosen`
        }
        onClick={() => (open ? close() : onOpenChange(true))}
      >
        {selected && (
          <span
            className={`inference-badge ${isLocal(selected) ? "local" : isApiKey(selected) ? "cloud api" : "cloud"}`}
          >
            {isLocal(selected)
              ? "Local"
              : selected.billing === "unknown"
                ? "Billing unverified"
                : selected.billing === "api_key" || isApiKey(selected)
                  ? "API key"
                  : "Cloud"}
          </span>
        )}
        <span className="unified-picker-current">
          {selected
            ? shortName(selected)
            : loading
              ? "Loading models…"
              : "Choose a model"}
        </span>
        <ChevronDown size={14} aria-hidden="true" />
      </button>
      {note && (
        <span className="picker-note" role="status">
          {note}
        </span>
      )}
      {open && (
        <div
          className="unified-picker-menu"
          role="dialog"
          aria-label="Choose a model"
        >
          <label className="unified-picker-search">
            <Search size={14} aria-hidden="true" />
            <input
              ref={search}
              value={query}
              role="combobox"
              aria-label="Search models"
              aria-expanded="true"
              aria-controls={listId}
              aria-autocomplete="list"
              aria-activedescendant={
                activeItem ? optionId(activeItem) : undefined
              }
              placeholder="Search models…"
              onChange={(e) => {
                setQuery(e.target.value);
                setActive(0);
                setDetails(null);
              }}
              onKeyDown={onKeyDown}
            />
          </label>
          <div
            className="unified-picker-list"
            role="listbox"
            id={listId}
            aria-label="Models"
          >
            {groups.map((group) => (
              <div
                key={group.id}
                className="unified-picker-group"
                role="group"
                aria-labelledby={`${uid}-${group.id}`}
                aria-describedby={
                  group.note ? `${uid}-${group.id}-note` : undefined
                }
              >
                <div
                  className="unified-picker-heading"
                  id={`${uid}-${group.id}`}
                  role="presentation"
                >
                  {group.title}
                </div>
                {group.note && (
                  <div
                    className="unified-picker-group-note"
                    id={`${uid}-${group.id}-note`}
                    role="presentation"
                  >
                    {group.note}
                  </div>
                )}
                {group.items.length === 0 && (
                  <div className="unified-picker-empty" role="presentation">
                    {group.empty}
                  </div>
                )}
                {group.items.map((item) => {
                  const index = items.indexOf(item);
                  const isActive = index === active;
                  if (item.kind === "add-key")
                    return (
                      <div
                        key={item.key}
                        id={optionId(item)}
                        role="option"
                        aria-selected={false}
                        className={`unified-picker-more unified-picker-add ${isActive ? "is-active" : ""}`}
                        onMouseDown={(e) => e.preventDefault()}
                        onMouseMove={() => setActive(index)}
                        onClick={() => activate(item)}
                      >
                        <KeyRound size={13} aria-hidden="true" />
                        {item.label}
                      </div>
                    );
                  if (item.kind === "more")
                    return (
                      <div
                        key={item.key}
                        id={optionId(item)}
                        role="option"
                        aria-selected={false}
                        className={`unified-picker-more ${isActive ? "is-active" : ""}`}
                        onMouseDown={(e) => e.preventDefault()}
                        onMouseMove={() => setActive(index)}
                        onClick={() => activate(item)}
                      >
                        <ChevronRight size={13} aria-hidden="true" />
                        Show all {item.hidden +
                          countShown(items, item.vendor)}{" "}
                        {item.label} models
                      </div>
                    );
                  return (
                    <Row
                      key={item.key}
                      id={optionId(item)}
                      target={item.target}
                      active={isActive}
                      selected={item.target.id === value}
                      detailsOpen={details === item.target.id}
                      detailsId={`${uid}-details`}
                      onHover={() => setActive(index)}
                      onPick={() => {
                        setActive(index);
                        activate(item);
                      }}
                      onDetails={() => {
                        setActive(index);
                        setDetails((d) =>
                          d === item.target.id ? null : item.target.id,
                        );
                        search.current?.focus();
                      }}
                    />
                  );
                })}
              </div>
            ))}
          </div>
          {detailTarget && (
            <Details
              id={`${uid}-details`}
              target={detailTarget}
              onConnect={(vendor) => {
                close(false);
                onConnect(vendor);
              }}
              onSetup={(target) => {
                close(false);
                onSetup(target);
              }}
            />
          )}
          <div className="unified-picker-actions">
            {onRefresh && (
              <button
                type="button"
                disabled={refreshing}
                onClick={async () => {
                  setRefreshing(true);
                  try {
                    await onRefresh();
                  } finally {
                    setRefreshing(false);
                    requestAnimationFrame(() => search.current?.focus());
                  }
                }}
              >
                <RefreshCw size={14} aria-hidden="true" />
                {refreshing ? "Checking models…" : "Refresh models"}
              </button>
            )}
            <button
              type="button"
              onClick={() => {
                close(false);
                onConnect();
              }}
            >
              <Plus size={14} aria-hidden="true" /> Connect account…
            </button>
            <button
              type="button"
              onClick={() => {
                close(false);
                onAddLocal();
              }}
            >
              <Plus size={14} aria-hidden="true" /> Add local model…
            </button>
          </div>
          <p className="unified-picker-keys" aria-hidden="true">
            ↑↓ move · Enter choose · → details · Esc close
          </p>
        </div>
      )}
    </div>
  );
}

function countShown(items: Item[], vendor: string) {
  return items.filter(
    (item) => item.kind === "row" && vendorKey(item.target) === vendor,
  ).length;
}

function Row({
  id,
  target,
  active,
  selected,
  detailsOpen,
  detailsId,
  onHover,
  onPick,
  onDetails,
}: {
  id: string;
  target: PickerTarget;
  active: boolean;
  selected: boolean;
  detailsOpen: boolean;
  detailsId: string;
  onHover: () => void;
  onPick: () => void;
  onDetails: () => void;
}) {
  const ready = isReady(target);
  return (
    <div
      id={id}
      role="option"
      aria-selected={selected}
      aria-describedby={detailsOpen ? `${id}-meta ${detailsId}` : `${id}-meta`}
      className={`unified-picker-row ${ready ? "" : "is-blocked"} ${selected ? "is-selected" : ""} ${active ? "is-active" : ""}`}
      onMouseDown={(e) => e.preventDefault()}
      onMouseMove={onHover}
      onClick={onPick}
    >
      <span className="unified-picker-copy">
        <span className="unified-picker-name">
          <strong>{target.name}</strong>
          {target.vision === true && (
            <span className="cap-badge" title="Accepts images">
              <Eye size={11} aria-hidden="true" /> Vision
            </span>
          )}
          {target.tools === false && (
            <span className="cap-badge" title="Answers only; cannot use tools">
              <MessageSquareText size={11} aria-hidden="true" /> Chat only
            </span>
          )}
        </span>
        <small id={`${id}-meta`}>
          {isLocal(target) ? "Local" : "Cloud"} ·{" "}
          <span className={`avail avail-${target.availability}`}>
            {availabilityLabel(target)}
          </span>
          {billingWarning(target) && ` · ${billingWarning(target)}`}
          {/* Rows show allowance only when the provider reports one; the
              "Usage unavailable" explanation stays in the details panel. */}
          {target.usage?.state !== "unavailable" &&
            rowUsage(availabilityLabel(target), target)}
        </small>
      </span>
      <span className="unified-picker-meta">
        {selected && <Check size={14} aria-hidden="true" />}
        {/* Keyboard users open the same details with the right arrow key;
            options cannot contain nested interactive controls. */}
        <span
          className={`picker-info ${detailsOpen ? "on" : ""}`}
          aria-hidden="true"
          title="Details"
          onClick={(e) => {
            e.stopPropagation();
            onDetails();
          }}
        >
          <Info size={13} />
        </span>
      </span>
    </div>
  );
}

function Details({
  id,
  target,
  onConnect,
  onSetup,
}: {
  id: string;
  target: PickerTarget;
  onConnect: (vendor: string) => void;
  onSetup: (target: PickerTarget) => void;
}) {
  const action = rowAction(target);
  const lines = usageDetailLines(target.usage);
  const apiKey = isApiKey(target);
  return (
    <div className="unified-picker-details" id={id} aria-live="polite">
      <strong>{target.name}</strong>
      {billingWarning(target) && <p>{billingWarning(target)}</p>}
      {apiKey && target.subtitle && <p>{target.subtitle}</p>}
      <p>
        {isLocal(target)
          ? "Runs on this computer"
          : apiKey
            ? "Cloud · your API key, billed per token"
            : "Cloud"}{" "}
        · {availabilityLabel(target)}
        {target.reason ? ` · ${target.reason}` : ""}
      </p>
      <p>{usageLabel(target.usage, target.inference)}</p>
      {lines.length > 0 && (
        <ul>
          {lines.map((line) => (
            <li key={line}>{line}</li>
          ))}
        </ul>
      )}
      {action.kind === "setup" && (
        <p className="unified-picker-hint">{action.hint}</p>
      )}
      <div className="row">
        {target.usage?.provider_usage_url && (
          <a
            href={target.usage.provider_usage_url}
            target="_blank"
            rel="noreferrer"
          >
            Open {vendorLabel(target)} {apiKey ? "activity" : "usage"}
          </a>
        )}
        {action.kind === "connect" && (
          <button
            type="button"
            className="mini"
            onClick={() => onConnect(action.vendor)}
          >
            {apiKey
              ? `Add ${vendorLabel(target)} API key`
              : `Sign in to ${vendorLabel(target)}`}
          </button>
        )}
        {action.kind === "setup" && (
          <button
            type="button"
            className="mini"
            onClick={() => onSetup(target)}
          >
            {action.local ? "Open Local models" : "Open Accounts"}
          </button>
        )}
      </div>
    </div>
  );
}

/** " · <usage>" for a row, or nothing when the usage only repeats the
 * availability. */
function rowUsage(availability: string, target: PickerTarget): string {
  const usage = usageAfterAvailability(
    availability,
    usageLabel(target.usage, target.inference),
  );
  return usage ? ` · ${usage}` : "";
}
