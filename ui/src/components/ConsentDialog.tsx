import { Dialog } from "./Dialog";
import type { ConsentRequest } from "../api";
import { providerLabel } from "../lib/transcript";

/** Asked before local conversation content or attachments go to a cloud
 * route. Nothing is sent until the user chooses Send. */
export function ConsentDialog({
  request,
  destination,
  billingWarning,
  attachments,
  onSend,
  onCancel,
}: {
  request: ConsentRequest;
  /** Picker row name of the destination, e.g. "Cursor · Auto". */
  destination?: string;
  /** Exact selected account warning; does not change the consent destination. */
  billingWarning?: string;
  /** Local attachment names in this message. */
  attachments: string[];
  onSend: () => void;
  onCancel: () => void;
}) {
  const handoff = request.handoff || {};
  const to = destination || providerLabel(handoff.to);
  const from = handoff.from ? providerLabel(handoff.from) : null;
  const chars = Number(handoff.excerpt_chars || 0);
  const images = Number(handoff.images || 0);
  return (
    <Dialog
      label="Send to a cloud provider?"
      className="modal modal-sm consent-dialog"
      onClose={onCancel}
    >
      <h2>Send to {to}?</h2>
      <p>
        This message goes to a cloud provider.{" "}
        {from
          ? `The conversation so far ran on ${from}.`
          : "It includes content that is only on this computer."}
      </p>
      {billingWarning && <p className="warn-text">{billingWarning}</p>}
      <ul className="consent-list">
        <li>Your new message</li>
        {chars > 0 && (
          <li>
            A summary of this conversation ({chars.toLocaleString()} characters)
          </li>
        )}
        {images > 0 && (
          <li>
            {images} image{images === 1 ? "" : "s"}
          </li>
        )}
        {attachments.length > 0 && (
          <li>Attachments: {attachments.join(", ")}</li>
        )}
      </ul>
      <div className="row end">
        <button type="button" className="ghost" onClick={onCancel}>
          Cancel
        </button>
        <button type="button" className="primary" onClick={onSend}>
          Send
        </button>
      </div>
    </Dialog>
  );
}
