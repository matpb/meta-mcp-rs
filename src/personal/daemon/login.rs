//! Login, logout and cancel: proxied to the sidecar with the daemon keeping state and flags in step.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value, json};

use super::platform::PlatformRt;
use super::{Daemon, OpError};
use crate::personal::PersonalPlatform;
use crate::personal::protocol::CODE_NOT_LOGGED_IN;

const LOGIN_START_TIMEOUT: Duration = Duration::from_secs(60);
const LOGIN_FINISH_TIMEOUT: Duration = Duration::from_secs(120);
const LOGIN_CANCEL_TIMEOUT: Duration = Duration::from_secs(30);
const LOGOUT_TIMEOUT: Duration = Duration::from_secs(60);

impl Daemon {
    /// Spawns the sidecar when absent (hello without state), allows state writes, proxies.
    pub(super) async fn login_start(
        self: &Arc<Self>,
        rt: &Arc<PlatformRt>,
        params: Map<String, Value>,
    ) -> Result<Value, OpError> {
        if !params.get("method").is_some_and(Value::is_string) {
            return Err(OpError::bad_params("method (string) is required"));
        }
        // The sidecar refuses sends until restarted, so a login on it would leave sends broken.
        if let Some(blocked) = rt.blocked_error() {
            return Err(blocked);
        }
        let sc = {
            let _life = rt.lifecycle.lock().await;
            {
                let _io = rt.state_io.lock().await;
                rt.inner().accept_state = true;
            }
            match rt.current() {
                Some((sc, _)) => sc,
                None => {
                    self.start_sidecar_locked(rt, None)
                        .await
                        .map_err(|e| e.error)?;
                    rt.current()
                        .map(|(sc, _)| sc)
                        .ok_or_else(|| OpError::sidecar_unavailable(rt.platform))?
                }
            }
        };
        let result = sc
            .link()
            .request_with_timeout("login_start", Value::Object(params), LOGIN_START_TIMEOUT)
            .await;
        self.flush_events(rt).await;
        Ok(result?)
    }

    /// Proxies; a finished Messenger login is connected right away.
    pub(super) async fn login_finish(
        self: &Arc<Self>,
        rt: &Arc<PlatformRt>,
        params: Map<String, Value>,
    ) -> Result<Value, OpError> {
        let (sc, epoch) = rt
            .current()
            .ok_or_else(|| OpError::sidecar_unavailable(rt.platform))?;
        let result = sc
            .link()
            .request_with_timeout("login_finish", Value::Object(params), LOGIN_FINISH_TIMEOUT)
            .await;
        self.flush_events(rt).await;
        let result = result?;
        if rt.platform == PersonalPlatform::Messenger
            && let Err(e) = self.connect_sidecar(rt, epoch).await
        {
            tracing::warn!(platform = rt.platform.as_str(), code = %e.code, "connect after login_finish failed");
        }
        Ok(result)
    }

    pub(super) async fn login_cancel(&self, rt: &Arc<PlatformRt>) -> Result<Value, OpError> {
        match rt.current() {
            Some((sc, _)) => Ok(sc
                .link()
                .request_with_timeout("login_cancel", Value::Null, LOGIN_CANCEL_TIMEOUT)
                .await?),
            None => Ok(json!({})),
        }
    }

    /// Asks the sidecar to log out, then always clears state, flags and the ring.
    pub(super) async fn logout(self: &Arc<Self>, rt: &Arc<PlatformRt>) -> Result<Value, OpError> {
        self.stop_reconnect(rt);
        {
            let _io = rt.state_io.lock().await;
            let mut g = rt.inner();
            g.accept_state = false;
            g.want_online = false;
        }
        let _life = rt.lifecycle.lock().await;
        let remote = match rt.current() {
            Some((sc, _)) => sc
                .link()
                .request_with_timeout("logout", Value::Null, LOGOUT_TIMEOUT)
                .await
                .err()
                .map(OpError::from),
            None => None,
        };
        self.flush_events(rt).await;
        let cleared = self.mark_logged_out(rt, Some("logout".to_owned())).await;
        {
            let mut g = rt.inner();
            g.sending_blocked = false;
            g.blocked_reason = None;
        }
        let mut out = json!({"state_cleared": cleared});
        if let Some(e) = remote {
            out["remote_error"] = json!({"code": e.code, "message": e.message});
        }
        Ok(out)
    }

    /// The sidecar behind a read or write op: logged in and running.
    pub(super) fn online(
        rt: &PlatformRt,
    ) -> Result<Arc<crate::personal::sidecar::Sidecar>, OpError> {
        let g = rt.inner();
        if !g.logged_in {
            return Err(OpError::new(
                CODE_NOT_LOGGED_IN,
                format!(
                    "not logged in to {}: run `meta-mcp personal login {}`",
                    rt.platform.as_str(),
                    rt.platform.as_str()
                ),
            ));
        }
        g.sidecar
            .clone()
            .ok_or_else(|| OpError::sidecar_unavailable(rt.platform))
    }
}
