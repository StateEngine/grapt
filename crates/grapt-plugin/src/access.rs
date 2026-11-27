//! Host-admitted filesystem roots and HTTP origins for one plugin process.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const BLOCKED_MSG: &str =
    "plugin_network_blocked: private, loopback, link-local and metadata addresses are denied";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PluginAccess {
    read: Vec<PathBuf>,
    write: Vec<PathBuf>,
    network: Vec<String>,
    #[cfg(any(test, feature = "loopback-testkit"))]
    #[serde(skip)]
    loopback_origins: Vec<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Declaration {
    read: Vec<String>,
    write: Vec<String>,
    network: Vec<String>,
}

impl PluginAccess {
    pub(crate) fn resolve(
        value: Option<&serde_json::Value>,
        directory: &Path,
    ) -> Result<Self, String> {
        let Some(value) = value else {
            return Ok(Self::default());
        };
        let declaration: Declaration = serde_json::from_value(value.clone()).map_err(|_| {
            "plugin permissions must declare read/write path lists and network HTTP(S) origins"
                .to_owned()
        })?;
        if declaration.read.len() + declaration.write.len() > 64 || declaration.network.len() > 64 {
            return Err(
                "plugin permissions exceed 64 filesystem roots or 64 network origins".into(),
            );
        }
        let mut network = Vec::new();
        for raw in declaration.network {
            let url = parse_url(&raw, false)?;
            if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
                return Err("plugin network permissions require exact HTTP(S) origins without paths, queries or fragments".into());
            }
            network.push(url.origin().ascii_serialization());
        }
        network.sort();
        network.dedup();
        Ok(Self {
            read: resolve_roots(declaration.read, directory)?,
            write: resolve_roots(declaration.write, directory)?,
            network,
            #[cfg(any(test, feature = "loopback-testkit"))]
            loopback_origins: Vec::new(),
        })
    }

    pub fn readable_roots(&self) -> &[PathBuf] {
        &self.read
    }
    pub fn writable_roots(&self) -> &[PathBuf] {
        &self.write
    }

    pub(crate) fn allows_loopback_origin(&self, origin: &str) -> bool {
        #[cfg(any(test, feature = "loopback-testkit"))]
        {
            self.loopback_origins.iter().any(|entry| entry == origin)
        }
        #[cfg(not(any(test, feature = "loopback-testkit")))]
        {
            let _ = origin;
            false
        }
    }

    /// Explicitly admit one exact loopback origin for test fixtures while
    /// keeping private, link-local, metadata, and all other origins blocked.
    #[cfg(any(test, feature = "loopback-testkit"))]
    #[doc(hidden)]
    pub fn allow_loopback_origin_for_tests(&mut self, raw: &str) -> Result<(), String> {
        let url = parse_url_shape(raw)?;
        let is_loopback_host = match url.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
            None => false,
        };
        if !is_loopback_host {
            return Err(
                "plugin loopback test grant accepts only loopback IP literals or localhost".into(),
            );
        }
        if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
            return Err("plugin network permissions require exact HTTP(S) origins without paths, queries or fragments".into());
        }
        let origin = url.origin().ascii_serialization();
        if !self.network.contains(&origin) {
            self.network.push(origin.clone());
            self.network.sort();
        }
        if !self.loopback_origins.contains(&origin) {
            self.loopback_origins.push(origin);
            self.loopback_origins.sort();
        }
        Ok(())
    }

    pub(crate) fn admit_url(&self, raw: &str) -> Result<(url::Url, bool), String> {
        let candidate = parse_url_shape(raw)?;
        let origin = candidate.origin().ascii_serialization();
        let allow_loopback = self.allows_loopback_origin(&origin);
        grapt_web::validate_public_host(&candidate, allow_loopback)
            .map_err(|_| BLOCKED_MSG.to_owned())?;
        if !self.network.contains(&origin) {
            return Err(format!("plugin_network_denied: add {origin} to this plugin's permissions.network in user-global or launch-explicit configuration"));
        }
        Ok((candidate, allow_loopback))
    }
}

fn resolve_roots(paths: Vec<String>, directory: &Path) -> Result<Vec<PathBuf>, String> {
    let mut roots = Vec::new();
    for raw in paths {
        if raw.is_empty() || raw.len() > 4096 || raw.contains(['\0', '*']) {
            return Err("plugin permission path is empty, wildcarded or oversized".to_owned());
        }
        let path = Path::new(&raw);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            directory.join(path)
        };
        let path = std::fs::canonicalize(path).map_err(|_| "plugin permission path is unavailable; create the intended directory before declaring it".to_owned())?;
        if path.parent().is_none() {
            return Err("plugin permission cannot grant a filesystem root; declare the specific workspace directory".to_owned());
        }
        roots.push(path);
    }
    roots.sort();
    roots.dedup();
    Ok(roots)
}

fn parse_url_shape(raw: &str) -> Result<url::Url, String> {
    if raw.len() > 8192 || raw.chars().any(char::is_control) {
        return Err("plugin_network_invalid: URL exceeds its bound or contains controls".into());
    }
    let url = url::Url::parse(raw)
        .map_err(|_| "plugin_network_invalid: use an absolute HTTP(S) URL".to_owned())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host_str().is_some_and(|host| host.contains('*'))
    {
        return Err("plugin_network_invalid: use a bounded HTTP(S) URL without credentials or wildcard hosts".into());
    }
    Ok(url)
}

fn parse_url(raw: &str, allow_loopback: bool) -> Result<url::Url, String> {
    let url = parse_url_shape(raw)?;
    grapt_web::validate_public_host(&url, allow_loopback).map_err(|_| BLOCKED_MSG.to_owned())?;
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn loopback_test_grant_rejects_non_loopback_origins() {
        let mut access = PluginAccess::default();
        for rejected in [
            "https://api.example".to_owned(),
            format!("https://{}", Ipv4Addr::new(93, 184, 216, 34)),
            format!("http://{}", Ipv4Addr::new(10, 0, 0, 1)),
            format!("http://{}", Ipv4Addr::new(169, 254, 169, 254)),
            "http://service.localhost:8080".to_owned(),
        ] {
            assert!(
                access.allow_loopback_origin_for_tests(&rejected).is_err(),
                "{rejected} must not acquire a loopback test grant"
            );
        }
        assert!(access
            .allow_loopback_origin_for_tests(&format!("http://{}:8080", Ipv4Addr::LOCALHOST))
            .is_ok());
        assert!(access
            .allow_loopback_origin_for_tests(&format!("http://[{}]:8080", Ipv6Addr::LOCALHOST))
            .is_ok());
        assert!(access
            .allow_loopback_origin_for_tests("http://localhost:8080")
            .is_ok());
    }
}
