use rusplay_config::{Config, ConfigEnvironment, ConfigError, ConfigRole};

pub const ROLES: [ConfigRole; 2] = [ConfigRole::Client, ConfigRole::Host];

pub const fn role_label(role: ConfigRole) -> &'static str {
    match role {
        ConfigRole::Client => "Client",
        ConfigRole::Host => "Host",
    }
}

pub fn load_baseline(
    environment: &ConfigEnvironment,
    role: ConfigRole,
) -> Result<Config, ConfigError> {
    let path = environment.config_path(role)?;

    Config::load_or_default(&path)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::*;

    fn scratch(name: &str) -> (ConfigEnvironment, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("rusplay-app-roles-{}-{name}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        fs::create_dir_all(root.join("rusplay")).unwrap();

        let environment = ConfigEnvironment {
            xdg_config_home: Some(root.clone()),
            home: None,
        };

        (environment, root)
    }

    #[test]
    fn a_missing_host_file_starts_from_defaults_not_from_the_client_file() {
        let (environment, root) = scratch("missing");
        fs::write(root.join("rusplay/client.toml"), "[network]\nport = 4242\n").unwrap();

        let client = load_baseline(&environment, ConfigRole::Client).unwrap();
        let host = load_baseline(&environment, ConfigRole::Host).unwrap();

        assert_eq!(client.network.port, 4242);
        assert_eq!(host, Config::default());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn each_role_reads_its_own_file() {
        let (environment, root) = scratch("own");
        fs::write(root.join("rusplay/client.toml"), "[network]\nport = 4242\n").unwrap();
        fs::write(root.join("rusplay/host.toml"), "[network]\nport = 5151\n").unwrap();

        let client = load_baseline(&environment, ConfigRole::Client).unwrap();
        let host = load_baseline(&environment, ConfigRole::Host).unwrap();

        assert_eq!(client.network.port, 4242);
        assert_eq!(host.network.port, 5151);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_malformed_host_file_is_an_error_and_not_a_silent_default() {
        let (environment, root) = scratch("malformed");
        fs::write(root.join("rusplay/host.toml"), "network = [").unwrap();

        let host = load_baseline(&environment, ConfigRole::Host);

        assert!(matches!(host, Err(ConfigError::Parse { .. })));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn both_roles_have_a_label() {
        assert_eq!(role_label(ConfigRole::Client), "Client");
        assert_eq!(role_label(ConfigRole::Host), "Host");
        assert_eq!(ROLES.len(), 2);
    }
}
