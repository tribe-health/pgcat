/// Parse the configuration file.
use arc_swap::ArcSwap;
use log::{error, info};
use once_cell::sync::Lazy;
use serde_derive::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use toml;

use crate::errors::Error;
use crate::tls::{load_certs, load_keys};
use crate::{ClientServerMap, ConnectionPool};

/// Globally available configuration.
static CONFIG: Lazy<ArcSwap<Config>> = Lazy::new(|| ArcSwap::from_pointee(Config::default()));

/// Server role: primary or replica.
#[derive(Clone, PartialEq, Deserialize, Hash, std::cmp::Eq, Debug, Copy)]
pub enum Role {
    Primary,
    Replica,
}

impl ToString for Role {
    fn to_string(&self) -> String {
        match *self {
            Role::Primary => "primary".to_string(),
            Role::Replica => "replica".to_string(),
        }
    }
}

impl PartialEq<Option<Role>> for Role {
    fn eq(&self, other: &Option<Role>) -> bool {
        match other {
            None => true,
            Some(role) => *self == *role,
        }
    }
}

impl PartialEq<Role> for Option<Role> {
    fn eq(&self, other: &Role) -> bool {
        match *self {
            None => true,
            Some(role) => role == *other,
        }
    }
}

/// Address identifying a PostgreSQL server uniquely.
#[derive(Clone, PartialEq, Hash, std::cmp::Eq, Debug)]
pub struct Address {
    pub id: usize,
    pub host: String,
    pub port: String,
    pub shard: usize,
    pub role: Role,
    pub replica_number: usize,
}

impl Default for Address {
    fn default() -> Address {
        Address {
            id: 0,
            host: String::from("127.0.0.1"),
            port: String::from("5432"),
            shard: 0,
            replica_number: 0,
            role: Role::Replica,
        }
    }
}

impl Address {
    /// Address name (aka database) used in `SHOW STATS`, `SHOW DATABASES`, and `SHOW POOLS`.
    pub fn name(&self) -> String {
        match self.role {
            Role::Primary => format!("shard_{}_primary", self.shard),

            Role::Replica => format!("shard_{}_replica_{}", self.shard, self.replica_number),
        }
    }
}

/// PostgreSQL user.
#[derive(Clone, PartialEq, Hash, std::cmp::Eq, Deserialize, Debug)]
pub struct User {
    pub name: String,
    pub password: String,
}

impl Default for User {
    fn default() -> User {
        User {
            name: String::from("postgres"),
            password: String::new(),
        }
    }
}

/// General configuration.
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct General {
    pub host: String,
    pub port: i16,
    pub pool_size: u32,
    pub pool_mode: String,
    pub connect_timeout: u64,
    pub healthcheck_timeout: u64,
    pub ban_time: i64,
    pub autoreload: bool,
    pub tls_certificate: Option<String>,
    pub tls_private_key: Option<String>,
}

impl Default for General {
    fn default() -> General {
        General {
            host: String::from("localhost"),
            port: 5432,
            pool_size: 15,
            pool_mode: String::from("transaction"),
            connect_timeout: 5000,
            healthcheck_timeout: 1000,
            ban_time: 60,
            autoreload: false,
            tls_certificate: None,
            tls_private_key: None,
        }
    }
}

/// Shard configuration.
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct Shard {
    pub servers: Vec<(String, u16, String)>,
    pub database: String,
}

impl Default for Shard {
    fn default() -> Shard {
        Shard {
            servers: vec![(String::from("localhost"), 5432, String::from("primary"))],
            database: String::from("postgres"),
        }
    }
}

/// Query Router configuration.
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct QueryRouter {
    pub default_role: String,
    pub query_parser_enabled: bool,
    pub primary_reads_enabled: bool,
    pub sharding_function: String,
}

impl Default for QueryRouter {
    fn default() -> QueryRouter {
        QueryRouter {
            default_role: String::from("any"),
            query_parser_enabled: false,
            primary_reads_enabled: true,
            sharding_function: "pg_bigint_hash".to_string(),
        }
    }
}

fn default_path() -> String {
    String::from("pgcat.toml")
}

/// Configuration wrapper.
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct Config {
    #[serde(default = "default_path")]
    pub path: String,

    pub general: General,
    pub user: User,
    pub shards: HashMap<String, Shard>,
    pub query_router: QueryRouter,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            path: String::from("pgcat.toml"),
            general: General::default(),
            user: User::default(),
            shards: HashMap::from([(String::from("1"), Shard::default())]),
            query_router: QueryRouter::default(),
        }
    }
}

impl From<&Config> for std::collections::HashMap<String, String> {
    fn from(config: &Config) -> HashMap<String, String> {
        HashMap::from([
            ("host".to_string(), config.general.host.to_string()),
            ("port".to_string(), config.general.port.to_string()),
            (
                "pool_size".to_string(),
                config.general.pool_size.to_string(),
            ),
            (
                "pool_mode".to_string(),
                config.general.pool_mode.to_string(),
            ),
            (
                "connect_timeout".to_string(),
                config.general.connect_timeout.to_string(),
            ),
            (
                "healthcheck_timeout".to_string(),
                config.general.healthcheck_timeout.to_string(),
            ),
            ("ban_time".to_string(), config.general.ban_time.to_string()),
            (
                "default_role".to_string(),
                config.query_router.default_role.to_string(),
            ),
            (
                "query_parser_enabled".to_string(),
                config.query_router.query_parser_enabled.to_string(),
            ),
            (
                "primary_reads_enabled".to_string(),
                config.query_router.primary_reads_enabled.to_string(),
            ),
            (
                "sharding_function".to_string(),
                config.query_router.sharding_function.to_string(),
            ),
        ])
    }
}

impl Config {
    /// Print current configuration.
    pub fn show(&self) {
        info!("Pool size: {}", self.general.pool_size);
        info!("Pool mode: {}", self.general.pool_mode);
        info!("Ban time: {}s", self.general.ban_time);
        info!(
            "Healthcheck timeout: {}ms",
            self.general.healthcheck_timeout
        );
        info!("Connection timeout: {}ms", self.general.connect_timeout);
        info!("Sharding function: {}", self.query_router.sharding_function);
        info!("Primary reads: {}", self.query_router.primary_reads_enabled);
        info!("Query router: {}", self.query_router.query_parser_enabled);
        info!("Number of shards: {}", self.shards.len());

        match self.general.tls_certificate.clone() {
            Some(tls_certificate) => {
                info!("TLS certificate: {}", tls_certificate);

                match self.general.tls_private_key.clone() {
                    Some(tls_private_key) => {
                        info!("TLS private key: {}", tls_private_key);
                        info!("TLS support is enabled");
                    }

                    None => (),
                }
            }

            None => {
                info!("TLS support is disabled");
            }
        };
    }
}

/// Get a read-only instance of the configuration
/// from anywhere in the app.
/// ArcSwap makes this cheap and quick.
pub fn get_config() -> Config {
    (*(*CONFIG.load())).clone()
}

/// Parse the configuration file located at the path.
pub async fn parse(path: &str) -> Result<(), Error> {
    let mut contents = String::new();
    let mut file = match File::open(path).await {
        Ok(file) => file,
        Err(err) => {
            error!("Could not open '{}': {}", path, err.to_string());
            return Err(Error::BadConfig);
        }
    };

    match file.read_to_string(&mut contents).await {
        Ok(_) => (),
        Err(err) => {
            error!("Could not read config file: {}", err.to_string());
            return Err(Error::BadConfig);
        }
    };

    let mut config: Config = match toml::from_str(&contents) {
        Ok(config) => config,
        Err(err) => {
            error!("Could not parse config file: {}", err.to_string());
            return Err(Error::BadConfig);
        }
    };

    match config.query_router.sharding_function.as_ref() {
        "pg_bigint_hash" => (),
        "sha1" => (),
        _ => {
            error!(
                "Supported sharding functions are: 'pg_bigint_hash', 'sha1', got: '{}'",
                config.query_router.sharding_function
            );
            return Err(Error::BadConfig);
        }
    };

    // Quick config sanity check.
    for shard in &config.shards {
        // We use addresses as unique identifiers,
        // let's make sure they are unique in the config as well.
        let mut dup_check = HashSet::new();
        let mut primary_count = 0;

        match shard.0.parse::<usize>() {
            Ok(_) => (),
            Err(_) => {
                error!(
                    "Shard '{}' is not a valid number, shards must be numbered starting at 0",
                    shard.0
                );
                return Err(Error::BadConfig);
            }
        };

        if shard.1.servers.len() == 0 {
            error!("Shard {} has no servers configured", shard.0);
            return Err(Error::BadConfig);
        }

        for server in &shard.1.servers {
            dup_check.insert(server);

            // Check that we define only zero or one primary.
            match server.2.as_ref() {
                "primary" => primary_count += 1,
                _ => (),
            };

            // Check role spelling.
            match server.2.as_ref() {
                "primary" => (),
                "replica" => (),
                _ => {
                    error!(
                        "Shard {} server role must be either 'primary' or 'replica', got: '{}'",
                        shard.0, server.2
                    );
                    return Err(Error::BadConfig);
                }
            };
        }

        if primary_count > 1 {
            error!("Shard {} has more than on primary configured", &shard.0);
            return Err(Error::BadConfig);
        }

        if dup_check.len() != shard.1.servers.len() {
            error!("Shard {} contains duplicate server configs", &shard.0);
            return Err(Error::BadConfig);
        }
    }

    match config.query_router.default_role.as_ref() {
        "any" => (),
        "primary" => (),
        "replica" => (),
        other => {
            error!(
                "Query router default_role must be 'primary', 'replica', or 'any', got: '{}'",
                other
            );
            return Err(Error::BadConfig);
        }
    };

    // Validate TLS!
    match config.general.tls_certificate.clone() {
        Some(tls_certificate) => {
            match load_certs(&Path::new(&tls_certificate)) {
                Ok(_) => {
                    // Cert is okay, but what about the private key?
                    match config.general.tls_private_key.clone() {
                        Some(tls_private_key) => match load_keys(&Path::new(&tls_private_key)) {
                            Ok(_) => (),
                            Err(err) => {
                                error!("tls_private_key is incorrectly configured: {:?}", err);
                                return Err(Error::BadConfig);
                            }
                        },

                        None => {
                            error!("tls_certificate is set, but the tls_private_key is not");
                            return Err(Error::BadConfig);
                        }
                    };
                }

                Err(err) => {
                    error!("tls_certificate is incorrectly configured: {:?}", err);
                    return Err(Error::BadConfig);
                }
            }
        }
        None => (),
    };

    config.path = path.to_string();

    // Update the configuration globally.
    CONFIG.store(Arc::new(config.clone()));

    Ok(())
}

pub async fn reload_config(client_server_map: ClientServerMap) -> Result<bool, Error> {
    let old_config = get_config();

    match parse(&old_config.path).await {
        Ok(()) => (),
        Err(err) => {
            error!("Config reload error: {:?}", err);
            return Err(Error::BadConfig);
        }
    };

    let new_config = get_config();

    if old_config.shards != new_config.shards || old_config.user != new_config.user {
        info!("Sharding configuration changed, re-creating server pools");
        ConnectionPool::from_config(client_server_map).await?;
        Ok(true)
    } else if old_config != new_config {
        Ok(true)
    } else {
        Ok(false)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn test_config() {
        parse("pgcat.toml").await.unwrap();
        assert_eq!(get_config().general.pool_size, 15);
        assert_eq!(get_config().shards.len(), 3);
        assert_eq!(get_config().shards["1"].servers[0].0, "127.0.0.1");
        assert_eq!(get_config().shards["0"].servers[0].2, "primary");
        assert_eq!(get_config().query_router.default_role, "any");
        assert_eq!(get_config().path, "pgcat.toml".to_string());
    }
}
