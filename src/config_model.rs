#![allow(dead_code)]

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(rename = "$schema", default)]
    #[schemars(rename = "$schema")]
    pub schema: Option<String>,
    #[serde(default)]
    pub proxy: ProxyConfig,
    #[serde(default)]
    pub contexts: ContextsConfig,
    #[serde(rename = "clusters", default)]
    pub clusters: BTreeMap<String, Cluster>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProxyConfig {
    #[serde(default = "default_proxy_port")]
    #[schemars(default = "default_proxy_port")]
    pub port: u16,
}

fn default_proxy_port() -> u16 {
    1355
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            port: default_proxy_port(),
        }
    }
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContextsConfig {
    #[serde(default)]
    pub ignore: Vec<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Cluster {
    #[serde(default)]
    pub context: Option<String>,
    #[serde(rename = "service", default)]
    pub services: Vec<Service>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Service {
    #[serde(default)]
    pub name: Option<String>,
    pub namespace: String,
    pub remote_port: u16,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub service: Option<String>,
    #[serde(default)]
    pub pod: Option<String>,
}
