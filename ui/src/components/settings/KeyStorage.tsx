import { useEffect, useState } from "react";
import { api, type SecretStorage } from "../../api";

const LABELS: Record<string, string> = {
  OPENROUTER_API_KEY: "OpenRouter API key",
};

/** Settings › Accounts: where each saved key lives (the secrets file or the
 * desktop keyring), and moving it between the two. Values are never shown. */
export function KeyStorage() {
  const [storage, setStorage] = useState<SecretStorage | null>(null);
  const [error, setError] = useState("");
  const [working, setWorking] = useState("");
  useEffect(() => {
    let live = true;
    Promise.resolve()
      .then(() => api.secretStorage())
      .then((s) => live && setStorage(s))
      .catch(() => live && setStorage(null));
    return () => {
      live = false;
    };
  }, []);
  if (!storage || storage.keys.length === 0) return null;
  async function move(name: string, to: "keyring" | "file") {
    setWorking(name);
    setError("");
    try {
      setStorage(await api.moveSecret(name, to));
    } catch (e) {
      setError(String(e));
    } finally {
      setWorking("");
    }
  }
  return (
    <section className="key-storage" aria-label="Where your keys are kept">
      <h4>Where your keys are kept</h4>
      <p className="hint">
        Keys are saved in a private file on this computer. On a desktop with a
        keyring (GNOME Keyring, KWallet, KeePassXC), you can keep them there
        instead.
        {!storage.keyring.available && storage.keyring.detail
          ? ` ${storage.keyring.detail}`
          : ""}
      </p>
      <ul className="always-list">
        {storage.keys.map((key) => (
          <li key={key.name}>
            <span>
              {LABELS[key.name] || <code>{key.name}</code>}
              <span className="hint">
                {" "}
                · {key.place === "keyring" ? "in the keyring" : "in the file"}
              </span>
            </span>
            {key.place === "file" ? (
              <button
                type="button"
                className="ghost"
                disabled={!storage.keyring.available || working === key.name}
                onClick={() => void move(key.name, "keyring")}
              >
                Move to keyring
              </button>
            ) : (
              <button
                type="button"
                className="ghost"
                disabled={working === key.name}
                onClick={() => void move(key.name, "file")}
              >
                Move back to the file
              </button>
            )}
          </li>
        ))}
      </ul>
      {error && <p className="warn-text">{error}</p>}
    </section>
  );
}
