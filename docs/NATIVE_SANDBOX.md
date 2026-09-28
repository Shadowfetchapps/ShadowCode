# Optional shell isolation

> **Reference.** This covers ShadowCode's own `exec` tool. For the everyday workflow see the [user guide](USER_GUIDE.md), [subscriptions](SUBSCRIPTIONS.md) and [local models](LOCAL_MODELS.md). The authoritative description of what each layer does and does not protect is in [SECURITY.md](../SECURITY.md#what-shadowcode-is-not).

Before an approved shell command runs, ShadowCode probes bubblewrap when it is
installed. The profile makes system directories read-only, replaces the home
folder with an empty temporary one (only the toolchain folders in
`sandbox.home_binds` come back, read-only, and credential folders are never
mounted), then binds the selected project read-write. The network namespace is
private unless shell network is on; in `allowlist` mode only the filtering proxy
is reachable. A private temporary directory is mounted at `/shadowcode-scratch`
and exposed as `SHADOWCODE_SCRATCH`.

The scratch directory is removed when the command returns. Cleanup accepts only
scratch directories created and retained by this process; it cannot delete an
arbitrary supplied path. It is ordinary temporary storage, **not copy-on-write**.
Approved commands still modify the live project, including its `.git` folder.
Checkpoints taken before each command let rewind restore project files; they do
not cover Git state or anything outside the project.

If bubblewrap is missing or its probe fails, the command is refused when
**Require sandbox** is on (and always in network `allowlist` mode). Otherwise it
runs under Landlock when the kernel supports it (file limits; TCP blocking from
Landlock ABI 4; Unix-socket and signal scoping from ABI 6 and 9), or without
isolation when neither layer is available. The conversation shows a warning.
Once execution begins, errors are returned without replaying the command
outside the sandbox. Doctor reports the probe result.

This optional profile is not a complete operating-system security boundary.
Commands run as your account. Review approvals and resulting changes.
