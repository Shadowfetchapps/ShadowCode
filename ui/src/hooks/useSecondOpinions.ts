import { useCallback, useEffect, useRef, useState } from "react";
import type { ConsentRequest } from "../api";
import {
  isActiveOpinion,
  opinionApi,
  type OpinionOptions,
  type OpinionScope,
  type SecondOpinion,
  type StartOpinion,
} from "../lib/secondOpinion";
import { isNative, listen } from "../lib/transport";
import type { ToastKind } from "./useToasts";

/** While a review runs, its record is read this often (an engine wake-up
 * reads it at once). */
export const OPINION_POLL_MS = 1500;

/** A cloud reviewer of work that ran on this computer: the consent dialog's
 * request and what Send and Cancel do. */
export type OpinionConsent = {
  request: ConsentRequest;
  send: () => void;
  cancel: () => void;
};

type Toast = (text: string, kind?: ToastKind) => void;

/** The second opinions of one scope (a conversation, a task or the staged
 * changes of a project), kept current while any runs, with start, stop,
 * dismiss and fix. Consent requests surface as `consent`. */
export function useSecondOpinions(scope: OpinionScope | null, toast: Toast) {
  const key = scope ? JSON.stringify(scope) : "";
  const scopeRef = useRef(scope);
  scopeRef.current = scope;
  const [items, setItems] = useState<SecondOpinion[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [consent, setConsent] = useState<OpinionConsent | null>(null);
  const [working, setWorking] = useState("");
  const request = useRef(0);

  const load = useCallback(async () => {
    const current = scopeRef.current;
    if (!current) return;
    const ticket = ++request.current;
    try {
      const listed = await opinionApi.list(current);
      if (ticket === request.current) {
        setItems(listed.second_opinions);
        setLoaded(true);
      }
    } catch {
      if (ticket === request.current) setLoaded(true);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);

  useEffect(() => {
    setItems([]);
    setLoaded(false);
    setConsent(null);
    void load();
  }, [load]);

  const active = items.some(isActiveOpinion);
  useEffect(() => {
    if (!active) return;
    const timer = setInterval(() => void load(), OPINION_POLL_MS);
    return () => clearInterval(timer);
  }, [active, load]);

  useEffect(() => {
    if (!key || !isNative()) return;
    let stop: (() => void) | undefined;
    let closed = false;
    void listen("shadowcode:events", (payload) => {
      const type = (payload as { type?: unknown } | null)?.type;
      if (type === "second_opinion.updated" || !type) void load();
    })
      .then((unlisten) => {
        if (closed) unlisten();
        else stop = unlisten;
      })
      .catch(() => undefined);
    return () => {
      closed = true;
      stop?.();
    };
  }, [key, load]);

  const upsert = useCallback((record: SecondOpinion) => {
    setItems((list) =>
      [record, ...list.filter((item) => item.id !== record.id)].sort(
        (a, b) => b.created_at - a.created_at,
      ),
    );
  }, []);

  /** Start one; resolves with the record, or null when refused or when
   * the user cancelled the consent dialog. */
  const start = useCallback(
    (body: StartOpinion): Promise<SecondOpinion | null> =>
      new Promise((resolve) => {
        const run = async (withConsent: boolean) => {
          setWorking("start");
          try {
            const answer = await opinionApi.start({
              ...body,
              consent: withConsent || undefined,
            });
            if ("consent" in answer) {
              if (withConsent) {
                toast("The second opinion could not start.", "err");
                resolve(null);
                return;
              }
              setConsent({
                request: answer.consent,
                send: () => {
                  setConsent(null);
                  void run(true);
                },
                cancel: () => {
                  setConsent(null);
                  resolve(null);
                },
              });
              return;
            }
            upsert(answer.second_opinion);
            resolve(answer.second_opinion);
          } catch (error) {
            toast(String(error), "err");
            resolve(null);
          } finally {
            setWorking("");
          }
        };
        void run(Boolean(body.consent));
      }),
    [toast, upsert],
  );

  const cancel = useCallback(
    async (id: string) => {
      setWorking(id);
      try {
        upsert(await opinionApi.cancel(id));
      } catch (error) {
        toast(String(error), "err");
      } finally {
        setWorking("");
      }
    },
    [toast, upsert],
  );

  const setFinding = useCallback(
    async (id: string, finding: string, status: "open" | "dismissed") => {
      try {
        upsert(await opinionApi.setFinding(id, finding, status));
      } catch (error) {
        toast(String(error), "err");
      }
    },
    [toast, upsert],
  );

  /** "Ask the agent to fix this": queue a follow-up in the conversation. */
  const fix = useCallback(
    (
      id: string,
      finding: string,
    ): Promise<{ id: string; session_id: string } | null> =>
      new Promise((resolve) => {
        const run = async (withConsent: boolean) => {
          setWorking(`${id}:${finding}`);
          try {
            const answer = await opinionApi.fix(id, finding, withConsent);
            if ("consent" in answer) {
              if (withConsent) {
                resolve(null);
                return;
              }
              setConsent({
                request: answer.consent,
                send: () => {
                  setConsent(null);
                  void run(true);
                },
                cancel: () => {
                  setConsent(null);
                  resolve(null);
                },
              });
              return;
            }
            upsert(answer.value.second_opinion);
            resolve(answer.value.job);
          } catch (error) {
            toast(String(error), "err");
            resolve(null);
          } finally {
            setWorking("");
          }
        };
        void run(false);
      }),
    [toast, upsert],
  );

  return {
    items,
    loaded,
    reload: load,
    start,
    cancel,
    setFinding,
    fix,
    consent,
    working,
  };
}

export type SecondOpinions = ReturnType<typeof useSecondOpinions>;

/** What the engine suggests for a reviewer of this scope (the saved
 * choice, offline mode, who wrote the change). */
export function useOpinionOptions(scope: OpinionScope | null) {
  const key = scope ? JSON.stringify(scope) : "";
  const [options, setOptions] = useState<OpinionOptions | null>(null);
  const scopeRef = useRef(scope);
  scopeRef.current = scope;
  const load = useCallback(async () => {
    const current = scopeRef.current;
    if (!current) return null;
    try {
      const next = await opinionApi.options(current);
      setOptions(next);
      return next;
    } catch {
      setOptions(null);
      return null;
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);
  useEffect(() => {
    setOptions(null);
    void load();
  }, [load]);
  return { options, reload: load, setOptions };
}
