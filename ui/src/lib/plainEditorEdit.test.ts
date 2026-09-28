import { expect, it } from "vitest";
import { plainEditorEdit } from "./plainEditorEdit";

it("keeps mixed file separators on unchanged textarea events and ordinary edits", () => {
  const original = "one\r\ntwo\rthree\nfour\r\n";
  expect(plainEditorEdit(original, "one\ntwo\nthree\nfour\n")).toBe(original);
  expect(plainEditorEdit(original, "one!\ntwo\nthree\nfour\n")).toBe(
    "one!\r\ntwo\rthree\nfour\r\n",
  );
  expect(plainEditorEdit(original, "one\ntwo\nthree\nfour!\n")).toBe(
    "one\r\ntwo\rthree\nfour!\r\n",
  );
});

it("changes only the selected span when deleting, inserting and replacing lines", () => {
  const original = "one\r\ntwo\rthree\nfour\r\n";
  expect(plainEditorEdit(original, "onetwo\nthree\nfour\n")).toBe(
    "onetwo\rthree\nfour\r\n",
  );
  expect(plainEditorEdit(original, "one\nnew\ntwo\nthree\nfour\n")).toBe(
    "one\r\nnew\ntwo\rthree\nfour\r\n",
  );
  expect(plainEditorEdit(original, "one\nreplacement\nfour\n")).toBe(
    "one\r\nreplacement\nfour\r\n",
  );
  expect(plainEditorEdit(original, "")).toBe("");
});
