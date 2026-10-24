//! Sidecar events: session state, connection tracking, inbox writes and fan-out to clients.

use std::sync::Arc;

use serde_json::{Value, json};

use super::inbox_map::inbox_events;
use super::platform::PlatformRt;
use super::ring::RecentEntry;
use super::sanitize_code as short;
use super::{Daemon, Incoming};
use crate::inbox::{self, EventKind};
use crate::personal::PersonalPlatform;
use crate::personal::protocol::{Account, Event};

/// `fatal` reasons after which the account may not send until `restart`.
const BLOCKING_REASONS: [&str; 6] = [
    "temporary_ban",
    "reachout_timelock",
    "banned",
    "device_removed",
    "client_outdated",
    "stream_replaced",
];

impl Daemon {
    pub(super) async fn on_event(self: &Arc<Self>, rt: &Arc<PlatformRt>, epoch: u64, ev: Event) {
        if rt.inner().generation != epoch {
            return;
        }
        match ev.event.as_str() {
            "ready" => return,
            "state_updated" => {
                self.on_state_updated(rt, &ev.data).await;
                return;
            }
            _ => {}
        }
        rt.inner().last_event_at = Some(inbox::now_secs());
        match ev.event.as_str() {
            "logged_in" => self.on_logged_in(rt, epoch, &ev.data),
            "logged_out" => self.on_logged_out(rt, &ev.data).await,
            "connection" => self.on_connection(rt, &ev.data),
            "message" | "history" | "reaction" | "receipt" => self.on_content(rt, &ev).await,
            "error" => {
                if let Some(code) = ev.data.get("code").and_then(Value::as_str) {
                    rt.inner().last_error = Some(short(code));
                }
            }
            _ => {}
        }
        self.forward(rt.platform, ev);
    }

    async fn on_state_updated(&self, rt: &Arc<PlatformRt>, data: &Value) {
        let _io = rt.state_io.lock().await;
        let state = data.get("state").filter(|s| !s.is_null());
        let result = match state {
            None => {
                let store = rt.store.clone();
                tokio::task::spawn_blocking(move || store.delete()).await
            }
            Some(state) => {
                if !rt.inner().accept_state {
                    tracing::warn!(
                        platform = rt.platform.as_str(),
                        "state_updated dropped: no login in progress"
                    );
                    return;
                }
                let (store, state) = (rt.store.clone(), state.clone());
                tokio::task::spawn_blocking(move || store.save(&state)).await
            }
        };
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::error!(platform = rt.platform.as_str(), error = %e, "state write failed");
                rt.inner().last_error = Some("state_write_failed".to_owned());
            }
            Err(_) => tracing::error!("state write task failed"),
        }
    }

    fn on_logged_in(self: &Arc<Self>, rt: &Arc<PlatformRt>, epoch: u64, data: &Value) {
        {
            let mut g = rt.inner();
            g.logged_in = true;
            g.want_online = true;
            g.last_error = None;
            if let Some(a) = data
                .get("account")
                .and_then(|a| serde_json::from_value::<Account>(a.clone()).ok())
            {
                g.account = Some(a);
            }
        }
        let daemon = Arc::clone(self);
        let rt = Arc::clone(rt);
        tokio::spawn(async move {
            if let Err(e) = daemon.connect_sidecar(&rt, epoch).await {
                tracing::warn!(platform = rt.platform.as_str(), code = %e.code, "connect after login failed");
            }
        });
    }

    /// The session ended: forget it, delete the state and stop reconnecting.
    async fn on_logged_out(self: &Arc<Self>, rt: &Arc<PlatformRt>, data: &Value) {
        let reason = data
            .get("reason")
            .and_then(Value::as_str)
            .map_or_else(|| "logged_out".to_owned(), short);
        tracing::warn!(platform = rt.platform.as_str(), %reason, "logged out by the service");
        self.stop_reconnect(rt);
        let _ = self.mark_logged_out(rt, Some(reason)).await;
    }

    /// Clears the session in memory and on disk; state writes stay refused until `login_start`.
    pub(super) async fn mark_logged_out(
        &self,
        rt: &Arc<PlatformRt>,
        reason: Option<String>,
    ) -> bool {
        let _io = rt.state_io.lock().await;
        {
            let mut g = rt.inner();
            g.logged_in = false;
            g.account = None;
            g.want_online = false;
            g.accept_state = false;
            g.connect_sent = false;
            g.backoff_idx = 0;
            g.connection = if g.sidecar.is_some() {
                "disconnected"
            } else {
                "stopped"
            };
            g.reason = reason;
            g.ring.clear();
        }
        let store = rt.store.clone();
        if let Ok(Ok(())) = tokio::task::spawn_blocking(move || store.delete()).await {
            true
        } else {
            tracing::error!(platform = rt.platform.as_str(), "state delete failed");
            rt.inner().last_error = Some("state_delete_failed".to_owned());
            false
        }
    }

    fn on_connection(self: &Arc<Self>, rt: &Arc<PlatformRt>, data: &Value) {
        let state = data.get("state").and_then(Value::as_str).unwrap_or("");
        let reason = data.get("reason").and_then(Value::as_str).map(short);
        let mut start_loop = false;
        let mut stop_loop = false;
        {
            let mut g = rt.inner();
            let connection = match state {
                "connecting" => "connecting",
                "connected" => "connected",
                "disconnected" => "disconnected",
                "fatal" => "fatal",
                _ => return,
            };
            g.connection = connection;
            g.reason.clone_from(&reason);
            match connection {
                "connected" => {
                    g.backoff_idx = 0;
                    g.reconnect_at = None;
                    g.last_error = None;
                }
                "disconnected" => g.connect_sent = false,
                "fatal" => {
                    g.connect_sent = false;
                    let blocking = reason
                        .as_deref()
                        .is_some_and(|r| BLOCKING_REASONS.contains(&r));
                    if blocking {
                        g.sending_blocked = true;
                        g.blocked_reason.clone_from(&reason);
                        g.last_error = reason.clone();
                        stop_loop = true;
                    } else {
                        g.last_error = Some(reason.clone().unwrap_or_else(|| "fatal".to_owned()));
                        start_loop = true;
                    }
                }
                _ => {}
            }
        }
        tracing::info!(
            platform = rt.platform.as_str(),
            state,
            reason = reason.as_deref().unwrap_or(""),
            "connection event"
        );
        if start_loop {
            self.start_reconnect_loop(rt);
        } else if stop_loop {
            self.stop_reconnect(rt);
        }
    }

    /// Inbox rows, the recent ring and the waiters.
    async fn on_content(self: &Arc<Self>, rt: &Arc<PlatformRt>, ev: &Event) {
        let account = rt
            .inner()
            .account
            .as_ref()
            .map(|a| a.id.clone())
            .unwrap_or_default();
        let rows = inbox_events(rt.platform, &account, ev);
        if rows.is_empty() {
            return;
        }
        if let Err(e) = self.inbox.insert_async(rows.clone()).await {
            tracing::error!(platform = rt.platform.as_str(), error = %e, "inbox write failed");
        }
        if ev.event != "message" {
            return;
        }
        for row in rows {
            let direction = if row.kind == EventKind::Message {
                "in"
            } else {
                "out"
            };
            let message_id = ev
                .data
                .get("message")
                .and_then(|m| super::inbox_map::id_of(m.get("id")))
                .unwrap_or_default();
            let fresh = rt.inner().ring.push(RecentEntry {
                message_id,
                id: row.id.clone(),
                platform: row.platform.as_str().to_owned(),
                thread_id: row.recipient.clone(),
                ts: row.ts,
                direction: direction.to_owned(),
            });
            if fresh && row.kind == EventKind::Message {
                let mut message = serde_json::to_value(&row).unwrap_or(Value::Null);
                if let Some(obj) = message.as_object_mut() {
                    obj.remove("raw");
                }
                // No receivers just means nobody is waiting.
                let _ = self.incoming.send(Arc::new(Incoming {
                    platform: rt.platform,
                    message,
                }));
            }
        }
    }

    /// Sends the event to subscribed connections with the platform added to its data.
    fn forward(&self, platform: PersonalPlatform, ev: Event) {
        let mut data = match ev.data {
            Value::Object(map) => map,
            Value::Null => serde_json::Map::new(),
            other => {
                let mut map = serde_json::Map::new();
                map.insert("value".to_owned(), other);
                map
            }
        };
        data.insert("platform".to_owned(), json!(platform.as_str()));
        let frame = json!({"event": ev.event, "data": data});
        if let Ok(line) = crate::personal::link::encode_line(&frame) {
            // No receivers just means no client subscribed.
            let _ = self.events.send(Arc::new(line));
        }
    }
}
