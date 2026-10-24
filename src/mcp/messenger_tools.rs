//! Messenger tools arrive in a later phase; this stub keeps the router seam in place.

use super::MetaMcp;

/// Tool names that send or change data on Messenger; removed in read-only mode.
pub const WRITE_TOOLS: &[&str] = &[];

#[rmcp::tool_router(router = messenger_router, vis = "pub", allow_empty)]
impl MetaMcp {}
