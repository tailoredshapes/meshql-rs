//! Stdio JSON-RPC transport for the meshql MCP server.
//!
//! `MeshqlMcpServer::serve_stdio` runs the request loop (one JSON-RPC request
//! per line on stdin, one response per line on stdout). Configuration —
//! server name/version, REST client, catalogue entity list, and any custom
//! tools — is supplied via [`McpServerConfig`].
