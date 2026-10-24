//! Discovery and probes: WhatsApp businesses and numbers, Page readability, scope comparison.

use serde_json::Value;

use crate::config::secret::Secret;
use crate::graph::token::DebugToken;
use crate::graph::{GraphClient, GraphError};
use crate::mcp::valid_id;

/// Scopes a Page token should carry for the Messenger tools.
pub const PAGE_TOKEN_SCOPES: [&str; 3] = [
    "pages_messaging",
    "pages_manage_metadata",
    "pages_read_engagement",
];

const PHONE_LIST_FIELDS: &str =
    "id,display_phone_number,verified_name,quality_rating,code_verification_status,platform_type";
const PHONE_CHECK_FIELDS: &str =
    "display_phone_number,verified_name,quality_rating,code_verification_status,status";
const LIST_CAP: usize = 200;

/// Graph codes that mean a permission or Page task is missing.
pub fn is_permission_code(code: Option<i64>) -> bool {
    matches!(code, Some(10 | 200 | 230))
}

/// What to grant when the conversations probe is refused for permission reasons.
pub const PERMISSION_NOTE: &str = "the token needs pages_messaging, pages_read_engagement and pages_manage_metadata, and the signed-in person needs the MESSAGING and MODERATE tasks on the Page";

/// Wanted scopes missing from `granted`, in the order wanted.
pub fn missing_scopes<'a>(
    wanted: impl IntoIterator<Item = &'a str>,
    granted: &[String],
) -> Vec<String> {
    wanted
        .into_iter()
        .filter(|w| !granted.iter().any(|g| g == w))
        .map(str::to_string)
        .collect()
}

/// Scopes from `scopes` plus those named under `granular_scopes`, without duplicates.
pub fn granted_scopes(d: &DebugToken) -> Vec<String> {
    let mut out = d.scopes.clone();
    let named = d.granular_scopes.as_array().into_iter().flatten();
    for scope in named.filter_map(|g| g.get("scope").and_then(Value::as_str)) {
        if !out.iter().any(|o| o == scope) {
            out.push(scope.to_string());
        }
    }
    out
}

/// True when scopes are known and one of the two WhatsApp permissions is missing.
pub fn lacks_whatsapp_scopes(granted: &[String]) -> bool {
    !granted.is_empty()
        && [
            "whatsapp_business_management",
            "whatsapp_business_messaging",
        ]
        .iter()
        .any(|s| !granted.iter().any(|g| g == s))
}

/// True when a task list is known and holds neither MESSAGING nor MANAGE.
pub fn lacks_messaging_task(tasks: &[String]) -> bool {
    !tasks.is_empty() && !tasks.iter().any(|t| t == "MESSAGING" || t == "MANAGE")
}

fn text(v: &Value, key: &str) -> Option<String> {
    match v.get(key)? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// A WhatsApp Business Account and the business it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Waba {
    pub id: String,
    pub name: String,
    pub business: String,
}

/// A business phone number as listed under its WABA.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Phone {
    pub id: String,
    pub display_phone_number: String,
    pub verified_name: String,
    pub quality_rating: Option<String>,
    pub code_verification_status: Option<String>,
    pub platform_type: Option<String>,
}

/// The accounts reachable with a token, plus the sub-calls that failed on the way.
#[derive(Debug, Clone, Default)]
pub struct Discovery {
    pub wabas: Vec<Waba>,
    pub notes: Vec<String>,
}

/// `me/businesses`, then owned and client WABAs of each; a failing edge becomes a note.
pub async fn discover_wabas(graph: &GraphClient, token: &Secret) -> Result<Discovery, GraphError> {
    let fields = [("fields", "id,name".to_string())];
    let businesses = graph
        .get_all("me/businesses", Some(token), &fields, LIST_CAP)
        .await?;
    let mut found = Discovery::default();
    for business in &businesses {
        let Some(business_id) = text(business, "id").filter(|id| valid_id(id)) else {
            continue;
        };
        let business_name = text(business, "name").unwrap_or_else(|| business_id.clone());
        for edge in [
            "owned_whatsapp_business_accounts",
            "client_whatsapp_business_accounts",
        ] {
            let path = format!("{business_id}/{edge}");
            match graph.get_all(&path, Some(token), &fields, LIST_CAP).await {
                Ok(items) => {
                    for item in &items {
                        let Some(id) = text(item, "id") else { continue };
                        if found.wabas.iter().any(|w| w.id == id) {
                            continue;
                        }
                        found.wabas.push(Waba {
                            name: text(item, "name").unwrap_or_else(|| id.clone()),
                            id,
                            business: business_name.clone(),
                        });
                    }
                }
                Err(e) => found
                    .notes
                    .push(format!("{edge} of business {business_name}: {e}")),
            }
        }
    }
    Ok(found)
}

/// Phone numbers of one WABA.
pub async fn phone_numbers(
    graph: &GraphClient,
    token: &Secret,
    waba_id: &str,
) -> Result<Vec<Phone>, GraphError> {
    let query = [("fields", PHONE_LIST_FIELDS.to_string())];
    let items = graph
        .get_all(
            &format!("{waba_id}/phone_numbers"),
            Some(token),
            &query,
            LIST_CAP,
        )
        .await?;
    Ok(items
        .iter()
        .filter_map(|p| {
            Some(Phone {
                id: text(p, "id")?,
                display_phone_number: text(p, "display_phone_number").unwrap_or_default(),
                verified_name: text(p, "verified_name").unwrap_or_default(),
                quality_rating: text(p, "quality_rating"),
                code_verification_status: text(p, "code_verification_status"),
                platform_type: text(p, "platform_type"),
            })
        })
        .collect())
}

/// What `GET {phone_number_id}` reports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhoneStatus {
    pub display_phone_number: String,
    pub verified_name: String,
    pub quality_rating: Option<String>,
    pub code_verification_status: Option<String>,
    pub status: Option<String>,
}

impl PhoneStatus {
    /// One-line summary such as `+1 555 0100 (Shop), quality GREEN, status CONNECTED`.
    pub fn summary(&self) -> String {
        let mut s = self.display_phone_number.clone();
        if !self.verified_name.is_empty() {
            s.push_str(&format!(" ({})", self.verified_name));
        }
        for (label, value) in [
            ("quality", &self.quality_rating),
            ("verification", &self.code_verification_status),
            ("status", &self.status),
        ] {
            if let Some(v) = value {
                s.push_str(&format!(", {label} {v}"));
            }
        }
        s
    }
}

pub async fn phone_status(
    graph: &GraphClient,
    token: &Secret,
    phone_number_id: &str,
) -> Result<PhoneStatus, GraphError> {
    let query = [("fields", PHONE_CHECK_FIELDS.to_string())];
    let v = graph.get(phone_number_id, Some(token), &query).await?;
    Ok(PhoneStatus {
        display_phone_number: text(&v, "display_phone_number").unwrap_or_default(),
        verified_name: text(&v, "verified_name").unwrap_or_default(),
        quality_rating: text(&v, "quality_rating"),
        code_verification_status: text(&v, "code_verification_status"),
        status: text(&v, "status"),
    })
}

/// Result of reading a Page's name and its conversation list.
pub struct PageProbe {
    pub name: Result<String, GraphError>,
    pub conversations: Result<(), GraphError>,
}

/// `GET {page}?fields=id,name` and `GET {page}/conversations?limit=1&platform=messenger`.
pub async fn probe_page(graph: &GraphClient, page_id: &str, token: &Secret) -> PageProbe {
    let name = graph
        .get(page_id, Some(token), &[("fields", "id,name".to_string())])
        .await
        .map(|v| text(&v, "name").unwrap_or_else(|| page_id.to_string()));
    let query = [
        ("limit", "1".to_string()),
        ("platform", "messenger".to_string()),
    ];
    let conversations = graph
        .get(&format!("{page_id}/conversations"), Some(token), &query)
        .await
        .map(|_| ());
    PageProbe {
        name,
        conversations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_comparison_keeps_the_wanted_order() {
        let granted = vec!["pages_messaging".to_string(), "pages_show_list".to_string()];
        assert_eq!(
            missing_scopes(PAGE_TOKEN_SCOPES, &granted),
            vec!["pages_manage_metadata", "pages_read_engagement"]
        );
        assert!(missing_scopes(["pages_messaging"], &granted).is_empty());
    }

    #[test]
    fn granular_scopes_extend_the_plain_list() {
        let d = DebugToken {
            app_id: "1".into(),
            user_id: None,
            is_valid: true,
            expires_at: None,
            issued_at: None,
            scopes: vec!["pages_messaging".into()],
            token_type: None,
            granular_scopes: serde_json::json!([
                {"scope": "pages_messaging", "target_ids": ["1"]},
                {"scope": "pages_show_list"}
            ]),
        };
        assert_eq!(
            granted_scopes(&d),
            vec!["pages_messaging", "pages_show_list"]
        );
        let bare = DebugToken {
            granular_scopes: serde_json::Value::Null,
            scopes: vec![],
            ..d
        };
        assert!(granted_scopes(&bare).is_empty());
    }

    #[test]
    fn whatsapp_scope_check_needs_known_scopes() {
        let some = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(
            !lacks_whatsapp_scopes(&[]),
            "unknown scopes are not a verdict"
        );
        assert!(lacks_whatsapp_scopes(&some(&["pages_messaging"])));
        assert!(lacks_whatsapp_scopes(&some(&[
            "whatsapp_business_management"
        ])));
        assert!(!lacks_whatsapp_scopes(&some(&[
            "whatsapp_business_management",
            "whatsapp_business_messaging"
        ])));
    }

    #[test]
    fn task_and_permission_helpers() {
        assert!(!lacks_messaging_task(&[]));
        assert!(!lacks_messaging_task(&[
            "MESSAGING".into(),
            "MODERATE".into()
        ]));
        assert!(!lacks_messaging_task(&["MANAGE".into()]));
        assert!(lacks_messaging_task(&["ADVERTISE".into()]));
        assert!(is_permission_code(Some(10)));
        assert!(is_permission_code(Some(230)));
        assert!(!is_permission_code(Some(190)));
        assert!(!is_permission_code(None));
    }

    #[test]
    fn phone_summary_lists_what_is_known() {
        let s = PhoneStatus {
            display_phone_number: "+1 555 0100".into(),
            verified_name: "Shop".into(),
            quality_rating: Some("GREEN".into()),
            code_verification_status: None,
            status: Some("CONNECTED".into()),
        };
        assert_eq!(
            s.summary(),
            "+1 555 0100 (Shop), quality GREEN, status CONNECTED"
        );
        assert_eq!(PhoneStatus::default().summary(), "");
    }
}
