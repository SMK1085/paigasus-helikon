//! AC1: connect to the upstream filesystem MCP server and load its tools.
//! Requires npm + network; run manually:
//! `cargo test -p paigasus-helikon-mcp --test acceptance_filesystem -- --ignored`

use paigasus_helikon_mcp::McpServerHandle;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires npm and network access; run locally before the PR"]
async fn filesystem_server_tools_load_with_schemas() {
    let dir = std::env::temp_dir();
    let handle = McpServerHandle::stdio(tokio::process::Command::new("npm"), |cmd| {
        // `npm exec --` is equivalent to `npx -y` but works reliably across all
        // Node.js shim managers (e.g. proto) whose `npx` shim misparses scoped
        // `@scope/pkg` arguments.  We also pin the public npm registry because
        // local machines may configure a private mirror (e.g. AWS CodeArtifact)
        // that does not carry @modelcontextprotocol packages.
        cmd.env("npm_config_registry", "https://registry.npmjs.org/")
            .arg("exec")
            .arg("--")
            .arg("@modelcontextprotocol/server-filesystem")
            .arg(dir);
    })
    .connect()
    .await
    .expect("connect to @modelcontextprotocol/server-filesystem");

    let tools = handle.tools::<()>();
    assert!(!tools.is_empty(), "filesystem server exposed no tools");
    let read_tool = tools
        .iter()
        .find(|t| t.name().to_ascii_lowercase().contains("read"))
        .expect("expected a read-style tool");
    assert!(read_tool.schema().is_object());
    assert!(!read_tool.description().is_empty());
    handle.close();
}
