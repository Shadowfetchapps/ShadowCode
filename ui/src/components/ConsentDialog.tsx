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
  // A second opinion or review sends material, not a new message.
  const opinion = handoff.purpose === "second_opinion";
  const files = Number(handoff.files || 0);
  return (
    <Dialog
      label="Send to a cloud provider?"
      className="modal modal-sm consent-dialog"
      onClose={onCancel}
    >
      <h2>Send to {to}?</h2>
      <p>
        {opinion
          ? "This second opinion goes to a cloud provider."
          : "This message goes to a cloud provider."}{" "}
        {from
          ? opinion
            ? `The work ran on ${from}, on this computer.`
            : `The conversation so far ran on ${from}.`
          : "It includes content that is only on this computer."}
      </p>
      {billingWarning && <p className="warn-text">{billingWarning}</p>}
      <ul className="consent-list">
        {opinion ? (
          <li>
            The request for the review
            {files > 0
              ? ` with the changes to ${files} file${files === 1 ? "" : "s"}`
              : ""}{" "}
            ({chars.toLocaleString()} characters). The reviewer can also read
            files in the project.
          </li>
        ) : (
          <li>Your new message</li>
        )}
        {!opinion && chars > 0 && (
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
