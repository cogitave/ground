use serde_json::Value;

use crate::ports::{PortError, PortResult, RegistryPort};

pub const NPM_REGISTRY: &str = "https://registry.npmjs.org";

/// `dist-tags.latest` of a packument, if it is a string.
pub fn parse_latest(body: &Value) -> Option<String> {
    body.get("dist-tags")?
        .get("latest")?
        .as_str()
        .map(str::to_owned)
}

/// Scoped names keep the '@' and escape only the slash, as npm does.
pub fn registry_url(pkg: &str, base: &str) -> String {
    format!("{base}/{}", pkg.replace('/', "%2F"))
}

/// Maps an HTTP answer to the port result: 404 is "no such package", other failures are errors.
pub fn interpret_latest(pkg: &str, status: u16, body: &[u8]) -> PortResult<Option<String>> {
    if status == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        // The registry is only ever read, so a 5xx/429 is worth asking again.
        return Err(PortError::from_http(
            true,
            &format!("npm registry for {pkg}"),
            status,
            None,
        ));
    }
    let json: Value = serde_json::from_slice(body)
        .map_err(|e| PortError::msg(format!("npm registry sent invalid JSON for {pkg}: {e}")))?;
    Ok(parse_latest(&json))
}

#[derive(Debug, Clone)]
pub struct NpmRegistry {
    client: reqwest::Client,
    base: String,
}

impl NpmRegistry {
    pub fn new() -> Self {
        Self::with_base_url(NPM_REGISTRY)
    }

    pub fn with_base_url(base: &str) -> Self {
        Self {
            client: crate::adapters::http_client(),
            base: base.trim_end_matches('/').to_owned(),
        }
    }
}

impl Default for NpmRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl RegistryPort for NpmRegistry {
    async fn latest_version(&self, pkg: &str) -> PortResult<Option<String>> {
        let res = self
            .client
            .get(registry_url(pkg, &self.base))
            .header("accept", "application/vnd.npm.install-v1+json")
            .send()
            .await
            .map_err(|e| PortError::from_transport(true, &e))?;
        let status = res.status().as_u16();
        let body = res
            .bytes()
            .await
            .map_err(|e| PortError::from_transport(true, &e))?;
        interpret_latest(pkg, status, &body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_dist_tags_latest() {
        assert_eq!(
            parse_latest(&json!({"dist-tags": {"latest": "1.2.3"}})).as_deref(),
            Some("1.2.3")
        );
        assert_eq!(parse_latest(&json!({})), None);
        assert_eq!(parse_latest(&Value::Null), None);
        assert_eq!(parse_latest(&json!({"dist-tags": {"latest": 3}})), None);
    }

    #[test]
    fn escapes_scoped_names() {
        assert_eq!(
            registry_url("@namzu/sdk", NPM_REGISTRY),
            "https://registry.npmjs.org/@namzu%2Fsdk"
        );
    }

    #[test]
    fn maps_404_to_none_errors_to_errors_and_success_to_the_version() {
        assert_eq!(interpret_latest("x", 404, b"{}").unwrap(), None);
        let body = br#"{"dist-tags":{"latest":"2.0.0"}}"#;
        assert_eq!(
            interpret_latest("x", 200, body).unwrap().as_deref(),
            Some("2.0.0")
        );
        assert!(
            interpret_latest("x", 500, b"{}")
                .unwrap_err()
                .to_string()
                .contains("500")
        );
        assert!(interpret_latest("x", 200, b"not json").is_err());
    }

    #[test]
    fn a_registry_5xx_or_429_is_transient_and_other_failures_are_not() {
        for status in [500, 502, 503, 429] {
            assert!(
                interpret_latest("x", status, b"{}")
                    .unwrap_err()
                    .is_transient(),
                "{status}"
            );
        }
        for status in [401, 403, 422] {
            assert!(
                !interpret_latest("x", status, b"{}")
                    .unwrap_err()
                    .is_transient(),
                "{status}"
            );
        }
        assert!(
            !interpret_latest("x", 200, b"not json")
                .unwrap_err()
                .is_transient()
        );
    }
}
