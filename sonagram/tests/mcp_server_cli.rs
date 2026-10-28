//! The MCP server binary's informational exits: `--version` is the line
//! `sonagram-mcp-server <version>` (KGLite's stale-build check parses its last
//! token), and `--help` usage names the real executable.

use std::process::Command;

fn run(arg: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_sonagram-mcp-server"))
        .arg(arg)
        .output()
        .expect("spawn sonagram-mcp-server");
    assert!(output.status.success(), "{arg} exited {:?}", output.status);
    String::from_utf8(output.stdout).expect("utf-8 stdout")
}

#[test]
fn version_line_is_the_binary_name_and_the_sonagram_version() {
    assert_eq!(
        run("--version").trim(),
        format!("sonagram-mcp-server {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn help_usage_names_the_executable() {
    let out = run("--help");
    let usage = out
        .lines()
        .find(|line| line.starts_with("Usage:"))
        .unwrap_or_else(|| panic!("no usage line:\n{out}"));
    assert!(usage.starts_with("Usage: sonagram-mcp-server "), "{usage}");
}
