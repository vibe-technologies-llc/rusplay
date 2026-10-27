use std::fs;

use rusplay_config::PeersConfig;
use rusplay_protocol::ClientName;

const HOSTNAME_FILE: &str = "/proc/sys/kernel/hostname";

pub(crate) fn announced_name(peers: &PeersConfig) -> ClientName {
    let hostname = fs::read_to_string(HOSTNAME_FILE).ok();

    choose(peers.client_name.as_deref(), hostname.as_deref())
}

fn choose(configured: Option<&str>, hostname: Option<&str>) -> ClientName {
    configured
        .or(hostname.map(str::trim))
        .map_or(ClientName::EMPTY, ClientName::lossy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_configured_name_wins_over_the_hostname() {
        let name = choose(Some("desk"), Some("machine\n"));

        assert_eq!(name.as_str(), "desk");
    }

    #[test]
    fn the_hostname_is_the_default_and_is_trimmed() {
        let name = choose(None, Some("machine\n"));

        assert_eq!(name.as_str(), "machine");
    }

    #[test]
    fn nothing_known_announces_no_name() {
        assert!(choose(None, None).is_empty());
    }

    #[test]
    fn a_long_hostname_is_cut_to_the_limit() {
        let name = choose(None, Some(&"h".repeat(200)));

        assert_eq!(name.as_bytes().len(), ClientName::MAX_LEN);
    }
}
