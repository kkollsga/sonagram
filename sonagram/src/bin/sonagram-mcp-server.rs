//! Thin native frontend for Sonagram's KGLite-composed MCP server.

fn main() {
    if let Err(error) = sonagram::mcp_server::run(std::env::args_os()) {
        eprintln!("sonagram-mcp-server: {error}");
        let mut source = error.source();
        while let Some(cause) = source {
            eprintln!("  caused by: {cause}");
            source = cause.source();
        }
        std::process::exit(1);
    }
}
