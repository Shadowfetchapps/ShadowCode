import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { installFakeBackend } from "../../../e2e/fakeBackend";
import { AccountsPage, LoginLine } from "./AccountsPage";
import { LocalModelsPage } from "./LocalModelsPage";
import { PermissionsPage } from "./PreferencePages";

let fake: ReturnType<typeof installFakeBackend>;
beforeEach(() => {
  vi.stubEnv("VITE_SHADOW_TEST_TRANSPORT", "1");
  fake = installFakeBackend({ stepMs: 5 });
});
afterEach(() => {
  cleanup();
  vi.unstubAllEnvs();
  delete window.__SHADOW_TEST_TRANSPORT__;
});

it("Accounts: explicit unknown billing overrides legacy auth labels without inventing quota", async () => {
  Object.assign(fake.state.vendors.codex, {
    billing: "unknown",
    account: { auth_mode: "apiKey" },
    usage: { state: "unavailable", label: "Usage unavailable", windows: [] },
    usage_note: null,
  });
  render(<AccountsPage onChanged={vi.fn()} onToast={vi.fn()} />);
  const codex = await screen.findByRole("article", { name: "Codex" });
  expect(
    within(codex).getByText("Billing unverified · API charges may apply"),
  ).toBeTruthy();
  expect(
    within(codex).queryByText("API key login · billed per token"),
  ).toBeNull();
  expect(codex.textContent).not.toMatch(/\d+%|Shared plan usage|Pro plan/);
});

it("Accounts: status, usage windows, models and Connect for signed-out vendors", async () => {
  const onChanged = vi.fn();
  render(<AccountsPage onChanged={onChanged} onToast={vi.fn()} />);
  const codex = await screen.findByRole("article", { name: "Codex" });
  expect(within(codex).getByText("Ready")).toBeTruthy();
  expect(within(codex).getByText("codex-cli 0.155.0")).toBeTruthy();
  expect(within(codex).getByText("dev@example.com · Pro plan")).toBeTruthy();
  expect(within(codex).getByText(/Weekly · 2% left · resets in/)).toBeTruthy();
  expect(within(codex).getByText(/Last checked/)).toBeTruthy();
  expect(
    within(codex).getByRole("link", { name: "Open Codex usage" }),
  ).toBeTruthy();
  expect(within(codex).getByText("2 models available")).toBeTruthy();
  expect(within(codex).queryByRole("button", { name: "Connect" })).toBeNull();
  expect(
    within(codex).getByRole("button", { name: "Disconnect" }),
  ).toBeTruthy();

  const claude = screen.getByRole("article", { name: "Claude Code" });
  expect(within(claude).getByText("Sign in")).toBeTruthy();
  expect(
    within(claude).queryByRole("button", { name: "Disconnect" }),
  ).toBeNull();
  fireEvent.click(within(claude).getByRole("button", { name: "Connect" }));
  // Login lines stream in with links and a selectable device code.
  const link = await within(claude).findByRole(
    "link",
    { name: /claude\.ai\/oauth/ },
    { timeout: 4000 },
  );
  expect(link.getAttribute("href")).toContain(
    "https://claude.ai/oauth/authorize",
  );
  await waitFor(
    () => expect(within(claude).getByText("WXYZ-1234").tagName).toBe("CODE"),
    { timeout: 4000 },
  );
  await waitFor(
    () => expect(within(claude).getByText("Signed in.")).toBeTruthy(),
    {
      timeout: 5000,
    },
  );
  await waitFor(() => expect(within(claude).getByText("Ready")).toBeTruthy());
  expect(onChanged).toHaveBeenCalled();
});

it("Accounts: Disconnect asks first and shows the shared CLI note", async () => {
  render(<AccountsPage onChanged={vi.fn()} onToast={vi.fn()} />);
  const codex = await screen.findByRole("article", { name: "Codex" });
  fireEvent.click(within(codex).getByRole("button", { name: "Disconnect" }));
  const dialog = screen.getByRole("dialog", { name: "Disconnect Codex" });
  expect(
    within(dialog).getByText(
      /signs out the codex CLI for your whole user account/,
    ),
  ).toBeTruthy();
  expect(fake.log.some((r) => r.path.endsWith("/disconnect"))).toBe(false);
  fireEvent.click(within(dialog).getByRole("button", { name: "Disconnect" }));
  await waitFor(() =>
    expect(
      fake.log.find((r) => r.path === "/api/accounts/codex/disconnect")?.body,
    ).toEqual({ confirm: true }),
  );
  await waitFor(() => expect(within(codex).getByText("Sign in")).toBeTruthy());
});

it("renders login output links and device codes safely", () => {
  render(
    <p>
      <LoginLine line="Visit https://example.com/device and enter ABCD-EFGH now" />
    </p>,
  );
  expect(screen.getByRole("link").getAttribute("href")).toBe(
    "https://example.com/device",
  );
  expect(screen.getByText("ABCD-EFGH").tagName).toBe("CODE");
});

it("Local models: runtime, hardware, compatibility, memory and actions", async () => {
  const onChanged = vi.fn();
  render(<LocalModelsPage onChanged={onChanged} onToast={vi.fn()} />);
  expect(await screen.findByText(/Ready · Vulkan · b6500/)).toBeTruthy();
  expect(
    screen.getByText(
      /16 CPU threads · 62 GB RAM · NVIDIA GeForce RTX 5060 Ti \(16 GB\)/,
    ),
  ).toBeTruthy();
  expect(screen.getByText("No model loaded")).toBeTruthy();
  const qwen = screen.getByRole("article", { name: "qwen3:14b" });
  expect(within(qwen).getByText("Compatible")).toBeTruthy();
  expect(within(qwen).getByText(/Needs about 11 GB/)).toBeTruthy();
  expect(within(qwen).getByText(/fits in GPU memory/)).toBeTruthy();
  const gptoss = screen.getByRole("article", { name: "gpt-oss:20b" });
  expect(within(gptoss).getByText("Not compatible")).toBeTruthy();
  expect(
    within(gptoss).getByText(/unknown model architecture: gptoss/),
  ).toBeTruthy();
  expect(within(gptoss).getByText("Chat only")).toBeTruthy();
  expect(within(gptoss).getByRole("button", { name: "Load" })).toHaveProperty(
    "disabled",
    true,
  );

  fireEvent.click(within(qwen).getByRole("button", { name: "Load" }));
  await waitFor(() =>
    expect(screen.getByText(/qwen3:14b · Vulkan0/)).toBeTruthy(),
  );
  expect(within(qwen).getByText("Loaded")).toBeTruthy();
  expect(onChanged).toHaveBeenCalled();

  // Ollama store: already-added rows are marked, others import in place.
  expect(screen.getByText("Added")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Import" }));
  expect(
    await screen.findByRole("article", { name: "gemma-4:12b" }),
  ).toBeTruthy();
  fireEvent.click(
    within(screen.getByRole("article", { name: "gemma-4:12b" })).getByRole(
      "button",
      {
        name: "Remove",
      },
    ),
  );
  await waitFor(() =>
    expect(screen.queryByRole("article", { name: "gemma-4:12b" })).toBeNull(),
  );
});

it("Permissions: saves only permissions and network groups", async () => {
  const onSave = vi.fn(async () => undefined);
  render(
    <PermissionsPage
      cfg={{
        permissions: {
          mode: "ask",
          level: "workspace",
          vendor_notes: { codex: "Codex sandbox note" },
        },
        network: { mode: "online" },
        ui: { theme: "dark" },
      }}
      onSave={onSave}
    />,
  );
  expect(screen.getByText("Codex sandbox note")).toBeTruthy();
  fireEvent.click(screen.getByLabelText(/Allow project edits/));
  fireEvent.click(screen.getByLabelText(/^Offline/));
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(onSave).toHaveBeenCalled());
  const values = (onSave.mock.calls[0] as unknown[])[0] as Record<
    string,
    unknown
  >;
  expect(Object.keys(values).sort()).toEqual([
    "network",
    "permissions",
    "sandbox",
  ]);
  expect(values).toMatchObject({
    permissions: { mode: "allow_edits", level: "workspace", network: false },
    network: { mode: "offline", shell: "off", allow: [] },
    sandbox: { require: false },
  });
});

it("Permissions: Require sandbox and the shell host allow-list", async () => {
  const onSave = vi.fn(async () => {});
  render(
    <PermissionsPage
      cfg={{
        permissions: { mode: "ask", level: "workspace", network: true },
        network: { mode: "online", shell: "on", allow: [] },
        sandbox: { require: false },
      }}
      onSave={onSave}
    />,
  );
  expect(await screen.findByRole("status")).toHaveProperty(
    "textContent",
    expect.stringContaining("Sandbox active (bubblewrap)"),
  );
  expect(screen.queryByLabelText("Allowed hosts")).toBeNull();
  fireEvent.click(screen.getByLabelText(/Require sandbox/));
  fireEvent.click(screen.getByLabelText(/Only allowed hosts/));
  fireEvent.change(screen.getByLabelText("Allowed hosts"), {
    target: { value: "crates.io\n *.githubusercontent.com \n\nlocalhost:3000" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(onSave).toHaveBeenCalled());
  expect((onSave.mock.calls[0] as unknown[])[0]).toMatchObject({
    permissions: { network: true },
    network: {
      mode: "online",
      shell: "allowlist",
      allow: ["crates.io", "*.githubusercontent.com", "localhost:3000"],
    },
    sandbox: { require: true },
  });
});

it("Accounts: OpenRouter key is validated, saved, never shown and removable", async () => {
  const onChanged = vi.fn();
  render(<AccountsPage onChanged={onChanged} onToast={vi.fn()} />);
  const card = await screen.findByRole("article", { name: "OpenRouter" });
  expect(within(card).getByText("API key")).toBeTruthy();
  await within(card).findByText("Add API key");
  expect(
    within(card)
      .getByRole("link", { name: "openrouter.ai/keys" })
      .getAttribute("href"),
  ).toBe("https://openrouter.ai/keys");
  const input = within(card).getByLabelText(
    "OpenRouter API key",
  ) as HTMLInputElement;
  expect(input.type).toBe("password");
  expect(input.getAttribute("autocomplete")).toBe("off");
  const save = within(card).getByRole("button", { name: "Save" });
  expect(save).toHaveProperty("disabled", true);

  // A rejected key: the engine's reason inline, nothing saved.
  fireEvent.change(input, { target: { value: "sk-or-wrong" } });
  fireEvent.click(save);
  const alert = await within(card).findByRole("alert");
  expect(alert.textContent).toBe("OpenRouter rejected this key (401)");
  expect(input.getAttribute("aria-invalid")).toBe("true");
  expect(onChanged).not.toHaveBeenCalled();

  fireEvent.change(input, { target: { value: "sk-or-valid" } });
  expect(within(card).queryByRole("alert")).toBeNull();
  fireEvent.click(within(card).getByRole("button", { name: "Save" }));
  await within(card).findByText("Key: sk-or-v1-a1b…9f2");
  expect(
    fake.log.find(
      (r) =>
        r.path === "/api/openrouter/key" && r.body.api_key === "sk-or-valid",
    ),
  ).toBeTruthy();
  expect(onChanged).toHaveBeenCalledTimes(1);
  // The field is gone and the key appears nowhere on the page.
  expect(within(card).queryByLabelText("OpenRouter API key")).toBeNull();
  expect(document.body.innerHTML).not.toContain("sk-or-valid");
  expect(within(card).getByText("Ready")).toBeTruthy();
  expect(
    within(card).getByText("Credits used: $1.23 of $10.00 limit ($8.77 left)"),
  ).toBeTruthy();
  expect(within(card).getByText(/40 models \(30 support tools\)/)).toBeTruthy();
  expect(within(card).getByText("Billed per token by OpenRouter")).toBeTruthy();
  expect(
    within(card)
      .getByRole("link", { name: "Open OpenRouter activity" })
      .getAttribute("href"),
  ).toBe("https://openrouter.ai/activity");
  await waitFor(() =>
    expect(document.activeElement?.textContent).toBe("Refresh models"),
  );

  fireEvent.click(within(card).getByRole("button", { name: "Refresh models" }));
  await waitFor(() =>
    expect(fake.log.some((r) => r.path === "/api/openrouter/refresh")).toBe(
      true,
    ),
  );
  await waitFor(() => expect(onChanged).toHaveBeenCalledTimes(2));

  // Remove asks first, like Disconnect.
  fireEvent.click(within(card).getByRole("button", { name: "Remove key" }));
  const dialog = screen.getByRole("dialog", { name: "Remove OpenRouter key" });
  expect(
    fake.log.some(
      (r) => r.path === "/api/openrouter/key" && r.body.api_key === "",
    ),
  ).toBe(false);
  fireEvent.click(within(dialog).getByRole("button", { name: "Remove key" }));
  const again = await within(card).findByLabelText("OpenRouter API key");
  expect((again as HTMLInputElement).value).toBe("");
  expect(
    fake.log.some(
      (r) => r.path === "/api/openrouter/key" && r.body.api_key === "",
    ),
  ).toBe(true);
  expect(onChanged).toHaveBeenCalledTimes(3);
});

it("Accounts: opening for OpenRouter focuses the key field; offline disables it", async () => {
  const { unmount } = render(
    <AccountsPage
      focusVendor="openrouter"
      onChanged={vi.fn()}
      onToast={vi.fn()}
    />,
  );
  const input = await screen.findByLabelText("OpenRouter API key");
  await waitFor(() => expect(document.activeElement).toBe(input));
  unmount();

  fake.state.config.network.mode = "offline";
  render(<AccountsPage onChanged={vi.fn()} onToast={vi.fn()} />);
  const card = await screen.findByRole("article", { name: "OpenRouter" });
  await within(card).findByText("Offline mode: OpenRouter is off.");
  expect(within(card).getByLabelText("OpenRouter API key")).toHaveProperty(
    "disabled",
    true,
  );
  expect(within(card).getByRole("button", { name: "Save" })).toHaveProperty(
    "disabled",
    true,
  );
});

const agentCard = () => screen.findByRole("article", { name: "Antigravity" });

it("Accounts: installing the Antigravity agent asks first, shows progress and survives reopening", async () => {
  const onChanged = vi.fn();
  const view = render(
    <AccountsPage
      installPollMs={150}
      onChanged={onChanged}
      onToast={vi.fn()}
    />,
  );
  const card = await agentCard();
  expect(within(card).getByText("Setup required")).toBeTruthy();
  expect(
    within(card).getByText(
      /runs through Google's official agent server, which asks ShadowCode before it runs commands or edits files/,
    ),
  ).toBeTruthy();
  expect(within(card).queryByRole("button", { name: "Connect" })).toBeNull();
  expect(document.body.textContent).not.toMatch(/Run agy/i);

  fireEvent.click(
    within(card).getByRole("button", { name: "Install Antigravity agent" }),
  );
  const dialog = screen.getByRole("dialog", {
    name: "Install the Antigravity agent",
  });
  expect(dialog.textContent).toContain("334 MB");
  expect(dialog.textContent).toContain("about 1.1 GB");
  expect(within(dialog).getByText("dl.google.com")).toBeTruthy();
  expect(
    within(dialog).getByText(
      "/home/user/.local/share/shadowcode/antigravity-acp/1.2.1",
    ),
  ).toBeTruthy();
  // Nothing is downloaded before the user confirms.
  expect(fake.log.some((r) => r.path.endsWith("/install"))).toBe(false);
  fireEvent.click(within(dialog).getByRole("button", { name: "Install" }));
  await waitFor(() =>
    expect(
      fake.log.find(
        (r) =>
          r.method === "POST" && r.path === "/api/accounts/antigravity/install",
      )?.body,
    ).toEqual({ confirm: true }),
  );
  expect(screen.queryByRole("dialog")).toBeNull();

  const bar = await within(card).findByRole("progressbar", {
    name: "Installing the Antigravity agent",
  });
  expect(bar.getAttribute("aria-valuemin")).toBe("0");
  expect(bar.getAttribute("aria-valuemax")).toBe("100");
  await within(card).findByText("Downloading 120 MB of 334 MB");
  expect(
    within(card).getByRole("progressbar").getAttribute("aria-valuenow"),
  ).toBe("36");

  // Closing and reopening Accounts picks the running install up again.
  view.unmount();
  render(
    <AccountsPage
      installPollMs={150}
      onChanged={onChanged}
      onToast={vi.fn()}
    />,
  );
  const again = await agentCard();
  await within(again).findByText("Checking the download…", undefined, {
    timeout: 3000,
  });
  await within(again).findByText("Unpacking…", undefined, { timeout: 3000 });
  const connect = await within(again).findByRole(
    "button",
    { name: "Connect" },
    { timeout: 3000 },
  );
  expect(within(again).getByText("Sign in")).toBeTruthy();
  expect(within(again).getByText("1.2.1")).toBeTruthy();
  expect(within(again).queryByRole("progressbar")).toBeNull();
  expect(
    fake.log.some(
      (r) =>
        r.method === "POST" && r.path === "/api/accounts/antigravity/refresh",
    ),
  ).toBe(true);
  expect(onChanged).toHaveBeenCalled();
  await waitFor(() => expect(document.activeElement).toBe(connect));
  // Polling stops once the install is done.
  const checks = fake.log.filter(
    (r) => r.method === "GET" && r.path === "/api/accounts/antigravity/install",
  ).length;
  await new Promise((resolve) => setTimeout(resolve, 400));
  expect(
    fake.log.filter(
      (r) =>
        r.method === "GET" && r.path === "/api/accounts/antigravity/install",
    ).length,
  ).toBe(checks);
});

it("Accounts: a failed Antigravity install shows the error and tries again", async () => {
  fake.state.agent.installFails = true;
  render(
    <AccountsPage installPollMs={20} onChanged={vi.fn()} onToast={vi.fn()} />,
  );
  const card = await agentCard();
  fireEvent.click(
    within(card).getByRole("button", { name: "Install Antigravity agent" }),
  );
  fireEvent.click(
    within(
      screen.getByRole("dialog", { name: "Install the Antigravity agent" }),
    ).getByRole("button", { name: "Install" }),
  );
  const alert = await within(card).findByRole("alert", undefined, {
    timeout: 3000,
  });
  expect(alert.textContent).toBe(
    "The download's SHA-256 does not match the published checksum",
  );
  expect(within(card).getByText("Setup required")).toBeTruthy();
  fireEvent.click(within(card).getByRole("button", { name: "Try again" }));
  await waitFor(() =>
    expect(
      fake.log.filter(
        (r) =>
          r.method === "POST" && r.path === "/api/accounts/antigravity/install",
      ),
    ).toHaveLength(2),
  );
  await within(card).findByRole(
    "button",
    { name: "Connect" },
    { timeout: 3000 },
  );
  expect(within(card).queryByRole("alert")).toBeNull();
});

it("Accounts: Remove agent asks first and returns Antigravity to setup", async () => {
  const v = fake.state.vendors.antigravity;
  Object.assign(v, {
    state: "ready",
    availability: "ready",
    availability_label: "Ready",
    version: "1.2.1",
    detail: "Signed in with Google",
    models: [
      { id: "gemini-3.5-pro", label: "Gemini 3.5 Pro", is_default: true },
    ],
    install: {
      ...v.install,
      installed: true,
      managed: true,
      state: "installed",
      path: "/home/user/.local/share/shadowcode/antigravity-acp/1.2.1/agy_acp_server.par",
    },
  });
  const onChanged = vi.fn();
  render(<AccountsPage onChanged={onChanged} onToast={vi.fn()} />);
  const card = await agentCard();
  expect(within(card).getByText("Ready")).toBeTruthy();
  expect(within(card).getByText("1 model available")).toBeTruthy();

  // Disconnect deletes ShadowCode's private profile, after the usual question.
  fireEvent.click(within(card).getByRole("button", { name: "Disconnect" }));
  const disconnect = screen.getByRole("dialog", {
    name: "Disconnect Antigravity (Google's ACP agent)",
  });
  expect(disconnect.textContent).toContain(
    "deletes ShadowCode's private Antigravity profile",
  );
  fireEvent.click(within(disconnect).getByRole("button", { name: "Cancel" }));

  fireEvent.click(within(card).getByRole("button", { name: "Remove agent" }));
  const dialog = screen.getByRole("dialog", {
    name: "Remove the Antigravity agent",
  });
  expect(
    within(dialog).getByText(
      "Frees about 1.1 GB. Your Antigravity sign-in is kept.",
    ),
  ).toBeTruthy();
  expect(fake.log.some((r) => r.path.endsWith("/uninstall"))).toBe(false);
  fireEvent.click(within(dialog).getByRole("button", { name: "Remove agent" }));
  await within(card).findByRole("button", {
    name: "Install Antigravity agent",
  });
  expect(
    fake.log.some(
      (r) =>
        r.method === "POST" && r.path === "/api/accounts/antigravity/uninstall",
    ),
  ).toBe(true);
  expect(within(card).getByText("Setup required")).toBeTruthy();
  expect(
    within(card).queryByRole("button", { name: "Remove agent" }),
  ).toBeNull();
  await waitFor(() => expect(onChanged).toHaveBeenCalled());
});

it("Accounts: an agent installed elsewhere has no Remove; offline disables Install", async () => {
  const v = fake.state.vendors.antigravity;
  v.install = {
    ...v.install,
    installed: true,
    managed: false,
    path: "/opt/agy/agy_acp_server.par",
  };
  Object.assign(v, {
    state: "not_logged_in",
    availability: "sign_in",
    availability_label: "Sign in",
  });
  const { unmount } = render(
    <AccountsPage onChanged={vi.fn()} onToast={vi.fn()} />,
  );
  const card = await agentCard();
  expect(within(card).getByRole("button", { name: "Connect" })).toBeTruthy();
  expect(
    within(card).queryByRole("button", { name: "Remove agent" }),
  ).toBeNull();
  unmount();

  v.install = { ...v.install, installed: false, managed: false, path: null };
  Object.assign(v, {
    state: "not_installed",
    availability: "setup_required",
    availability_label: "Setup required",
  });
  fake.state.config.network.mode = "offline";
  render(<AccountsPage offline onChanged={vi.fn()} onToast={vi.fn()} />);
  const offline = await agentCard();
  const install = within(offline).getByRole("button", {
    name: "Install Antigravity agent",
  });
  expect(install).toHaveProperty("disabled", true);
  expect(
    within(offline).getByText(/Offline mode: downloads are off/),
  ).toBeTruthy();
});

it("Local models: a file tool hint is not presented as verified support", async () => {
  Object.assign(fake.state.local.models[0], {
    tools: true,
    tools_basis: "template_hint",
    tools_reason: "Template mentions tools · file hint only",
  });
  render(<LocalModelsPage onChanged={vi.fn()} onToast={vi.fn()} />);
  const model = await screen.findByRole("article", { name: "qwen3:14b" });
  expect(within(model).getByText("Tool hint").getAttribute("title")).toBe(
    "Template mentions tools · file hint only",
  );
  expect(
    within(model).queryByText(/Tools verified|Tool support verified/),
  ).toBeNull();
});
