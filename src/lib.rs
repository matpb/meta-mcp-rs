//! MCP server for Facebook Messenger and WhatsApp: business mode on the Meta Graph API, and personal
//! accounts through local sidecars.

// `GraphError::Api` keeps Meta's whole error object inline; boxing it would change the variant shape.
#![allow(clippy::result_large_err)]

pub mod cli;
pub mod config;
pub mod files;
pub mod graph;
pub mod inbox;
pub mod mcp;
pub mod messenger;
pub mod personal;
pub mod server;
pub mod setup;
pub mod webhook;
pub mod whatsapp;
