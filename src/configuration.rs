use secrecy::Secret;
use serde_aux::prelude::deserialize_number_from_string;
use std::path::PathBuf;

#[derive(serde::Deserialize, Clone)]
pub struct Settings {
    pub application: ApplicationSettings,
    pub database: DatabaseSettings,
    pub admin: AdminSettings,
    pub aliases: AliasSettings,
    pub inbound: InboundSettings,
    pub delivery: DeliverySettings,
    #[serde(default)]
    pub dkim: DkimSettings,
}

#[derive(serde::Deserialize, Clone)]
pub struct ApplicationSettings {
    #[serde(deserialize_with = "deserialize_number_from_string")]
    pub port: u16,
    pub host: String,
    pub base_url: String,
    pub hmac_secret: Secret<String>,
}

#[derive(serde::Deserialize, Clone)]
pub struct DatabaseSettings {
    pub path: String,
}

impl DatabaseSettings {
    /// `mode=rwc` creates the database file when it does not exist yet.
    pub fn connection_string(&self) -> String {
        format!("sqlite://{}?mode=rwc", self.path)
    }
}

/// The single operator account. The password is applied on every boot, so the
/// configuration file (or `APP_ADMIN__PASSWORD`) is the source of truth.
#[derive(serde::Deserialize, Clone)]
pub struct AdminSettings {
    pub username: String,
    pub password: Secret<String>,
}

#[derive(serde::Deserialize, Clone)]
pub struct AliasSettings {
    /// Domains this instance accepts mail for. The first one is used for newly
    /// generated aliases and reverse aliases.
    pub domains: Vec<String>,
    /// Prefix of the local part of reverse aliases, e.g. `ct` in
    /// `ct.9f2a1b@example.com`.
    #[serde(default = "default_reverse_alias_prefix")]
    pub reverse_alias_prefix: String,
}

fn default_reverse_alias_prefix() -> String {
    "ct".to_string()
}

impl AliasSettings {
    pub fn default_domain(&self) -> &str {
        self.domains
            .first()
            .map(String::as_str)
            .expect("`aliases.domains` must contain at least one domain")
    }

    pub fn owns_domain(&self, domain: &str) -> bool {
        self.domains
            .iter()
            .any(|d| d.eq_ignore_ascii_case(domain.trim_end_matches('.')))
    }
}

#[derive(serde::Deserialize, Clone)]
pub struct InboundSettings {
    pub host: String,
    #[serde(deserialize_with = "deserialize_number_from_string")]
    pub port: u16,
    /// Greeting hostname announced in the SMTP banner and EHLO response.
    pub hostname: String,
    #[serde(default = "default_max_message_size")]
    pub max_message_size: usize,
}

fn default_max_message_size() -> usize {
    25 * 1024 * 1024
}

#[derive(serde::Deserialize, Clone)]
pub struct DeliverySettings {
    /// `relay` hands every message to a smarthost, `direct` looks up the MX of
    /// the recipient domain and talks to it directly.
    pub mode: DeliveryMode,
    #[serde(default)]
    pub relay: Option<RelaySettings>,
    /// Directory holding the raw `.eml` files referenced by the outbox.
    pub mail_dir: PathBuf,
    #[serde(default = "default_poll_interval_secs")]
    pub poll_interval_secs: u64,
    #[serde(default = "default_max_attempts")]
    pub max_attempts: i64,
    /// Hostname used in the EHLO of outgoing connections.
    pub ehlo_hostname: String,
}

fn default_poll_interval_secs() -> u64 {
    10
}

fn default_max_attempts() -> i64 {
    8
}

#[derive(serde::Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum DeliveryMode {
    Relay,
    Direct,
}

#[derive(serde::Deserialize, Clone)]
pub struct RelaySettings {
    pub host: String,
    #[serde(deserialize_with = "deserialize_number_from_string")]
    pub port: u16,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<Secret<String>>,
    /// Implicit TLS (port 465). STARTTLS is negotiated automatically otherwise.
    #[serde(default)]
    pub implicit_tls: bool,
}

#[derive(serde::Deserialize, Clone, Default)]
pub struct DkimSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub selector: String,
    /// PEM-encoded RSA private key.
    #[serde(default)]
    pub private_key_path: Option<PathBuf>,
}

pub enum Environment {
    Local,
    Production,
}

impl Environment {
    pub fn as_str(&self) -> &'static str {
        match self {
            Environment::Local => "local",
            Environment::Production => "production",
        }
    }
}

impl TryFrom<String> for Environment {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        match s.to_lowercase().as_str() {
            "local" => Ok(Self::Local),
            "production" => Ok(Self::Production),
            other => Err(format!(
                "{} is not a supported environment. Use either `local` or `production`",
                other
            )),
        }
    }
}

pub fn get_configuration() -> Result<Settings, config::ConfigError> {
    let base_path = std::env::current_dir().expect("Failed to determine the current directory");
    let configuration_directory = base_path.join("configuration");

    let environment: Environment = std::env::var("APP_ENVIRONMENT")
        .unwrap_or_else(|_| "local".into())
        .try_into()
        .expect("Failed to parse APP_ENVIRONMENT");

    let settings = config::Config::builder()
        .add_source(config::File::from(configuration_directory.join("base")).required(true))
        .add_source(
            config::File::from(configuration_directory.join(environment.as_str())).required(true),
        )
        // E.g. `APP_APPLICATION__PORT=5001` sets `Settings.application.port`.
        .add_source(
            config::Environment::with_prefix("APP")
                .prefix_separator("_")
                .separator("__")
                .list_separator(",")
                .with_list_parse_key("aliases.domains")
                .try_parsing(true),
        )
        .build()?;

    settings.try_deserialize::<Settings>()
}
