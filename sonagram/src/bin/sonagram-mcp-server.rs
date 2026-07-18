//! Thin native frontend for Sonagram's KGLite-composed MCP server.

fn main() {
    if let Err(error) = sonagram::mcp_server::run(std::env::args_os()) {
        eprintln!("sonagram-mcp-server: {error}");
        std::process::exit(1);
    }
}
