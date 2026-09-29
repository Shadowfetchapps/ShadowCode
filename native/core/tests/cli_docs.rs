//! The packaged manual page and shell completions come from the same clap
//! definitions as `--help`; packaging installs what these functions print.
use shadowcode_core::cli::{args::CompletionShell, completions, manpage};
use std::{io::Write, process::Command};

#[test]
fn completions_cover_every_supported_shell() {
    let bash = String::from_utf8(completions(CompletionShell::Bash).unwrap()).unwrap();
    assert!(bash.contains("_shadowcode()"), "{bash}");
    assert!(bash.contains("complete -F _shadowcode"));
    let zsh = String::from_utf8(completions(CompletionShell::Zsh).unwrap()).unwrap();
    assert!(zsh.starts_with("#compdef shadowcode"));
    let fish = String::from_utf8(completions(CompletionShell::Fish).unwrap()).unwrap();
    assert!(fish.contains("complete -c shadowcode"));
    for script in [&bash, &zsh, &fish] {
        // Subcommands and their options are completed.
        assert!(script.contains("worktree"));
        assert!(script.contains("completions"));
        assert!(script.contains("workspace"));
    }
}

#[test]
fn manual_page_lists_commands_files_and_the_update_switch() {
    let page = String::from_utf8(manpage().unwrap()).unwrap();
    assert!(page.contains(".TH SHADOWCODE 1"));
    assert!(page.contains(".SH COMMANDS"));
    assert!(page.contains("\\fBshadowcode run\\fR"));
    assert!(page.contains("\\fBshadowcode completions\\fR"));
    assert!(page.contains("\\fBshadowcode rules\\fR"));
    assert!(page.contains("~/.config/shadowcode/profile/"));
    // Only this page is installed: no references to per-command pages, and
    // the packaging-only command stays hidden.
    assert!(!page.contains("shadowcode\\-run(1)"));
    assert!(!page.contains("shadowcode manpage"));
    assert!(page.contains("/etc/shadowcode/policy.yaml"));
    assert!(page.contains("updates.check: false"));
    assert!(page.contains("originally created by Shadowfetch"));
    assert!(page.contains(shadowcode_core::VERSION));
    // groff, when installed, formats the page without a single warning.
    let Ok(mut groff) = Command::new("groff")
        .args(["-man", "-Tutf8", "-ww", "-z"])
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    else {
        return;
    };
    groff
        .stdin
        .take()
        .unwrap()
        .write_all(page.as_bytes())
        .unwrap();
    let output = groff.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
}
