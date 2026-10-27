use std::net::{IpAddr, SocketAddr};

use rusplay_client::{DiscoveredHost, PeerRecord};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HostError {
    #[error("{text:?} is not an IP address with an optional port")]
    InvalidAddress { text: String },
    #[error("{address} is already in the list")]
    Duplicate { address: SocketAddr },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostOrigin {
    Added,
    Discovered,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEntry {
    pub name: String,
    pub address: SocketAddr,
    pub peer: Option<PeerRecord>,
    pub origin: HostOrigin,
    pub online: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostList {
    default_port: u16,
    entries: Vec<HostEntry>,
    selected: Option<usize>,
}

impl HostList {
    pub const fn new(default_port: u16) -> Self {
        Self {
            default_port,
            entries: Vec::new(),
            selected: None,
        }
    }

    pub fn entries(&self) -> &[HostEntry] {
        &self.entries
    }

    pub const fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    pub fn selected(&self) -> Option<&HostEntry> {
        self.selected.and_then(|index| self.entries.get(index))
    }

    pub fn parse_address(&self, text: &str) -> Result<SocketAddr, HostError> {
        let trimmed = text.trim();

        trimmed
            .parse::<SocketAddr>()
            .or_else(|_| {
                trimmed
                    .trim_matches(['[', ']'])
                    .parse::<IpAddr>()
                    .map(|ip| SocketAddr::new(ip, self.default_port))
            })
            .map_err(|_| HostError::InvalidAddress {
                text: trimmed.to_owned(),
            })
    }

    pub fn add(&mut self, name: &str, address_text: &str) -> Result<usize, HostError> {
        let address = self.parse_address(address_text)?;

        if self.entries.iter().any(|entry| entry.address == address) {
            return Err(HostError::Duplicate { address });
        }

        let name = match name.trim() {
            "" => address.to_string(),
            named => named.to_owned(),
        };

        self.entries.push(HostEntry {
            name,
            address,
            peer: None,
            origin: HostOrigin::Added,
            online: false,
        });
        let index = self.entries.len() - 1;
        self.selected = Some(index);

        Ok(index)
    }

    pub fn select(&mut self, index: usize) {
        if index < self.entries.len() {
            self.selected = Some(index);
        }
    }

    pub fn remember_peer(&mut self, address: SocketAddr, peer: PeerRecord) -> bool {
        match self
            .entries
            .iter_mut()
            .find(|entry| entry.address == address)
        {
            Some(entry) => {
                entry.peer = Some(peer);
                entry.origin = HostOrigin::Added;
                true
            }
            None => false,
        }
    }

    pub fn merge_discovered(&mut self, found: &[DiscoveredHost]) {
        for entry in &mut self.entries {
            entry.online = false;
        }

        let mut unmatched = Vec::new();
        for host in found {
            match self
                .entries
                .iter_mut()
                .find(|entry| entry.address == host.address)
            {
                Some(entry) => {
                    entry.online = true;
                    if entry.origin == HostOrigin::Discovered {
                        entry.name.clone_from(&host.name);
                    }
                }
                None => unmatched.push(host),
            }
        }

        let mut new_hosts = Vec::new();
        for host in &unmatched {
            if self.move_saved_host(host, &unmatched) {
                continue;
            }
            new_hosts.push(HostEntry {
                name: host.name.clone(),
                address: host.address,
                peer: None,
                origin: HostOrigin::Discovered,
                online: true,
            });
        }

        let selected_address = self.selected().map(|entry| entry.address);
        self.entries
            .retain(|entry| entry.online || entry.origin == HostOrigin::Added);
        self.entries.extend(new_hosts);
        self.selected = selected_address.and_then(|address| {
            self.entries
                .iter()
                .position(|entry| entry.address == address)
        });
    }

    fn move_saved_host(&mut self, host: &DiscoveredHost, unmatched: &[&DiscoveredHost]) -> bool {
        let announced_once = unmatched
            .iter()
            .filter(|other| other.name == host.name)
            .count()
            == 1;
        if !announced_once {
            return false;
        }

        let mut candidates = self.entries.iter_mut().filter(|entry| {
            entry.origin == HostOrigin::Added && !entry.online && entry.name == host.name
        });
        match (candidates.next(), candidates.next()) {
            (Some(entry), None) => {
                entry.address = host.address;
                entry.online = true;
                true
            }
            _ => false,
        }
    }

    pub fn remove(&mut self, index: usize) {
        if index >= self.entries.len() {
            return;
        }

        self.entries.remove(index);
        self.selected = match self.selected {
            Some(selected) if selected == index => None,
            Some(selected) if selected > index => Some(selected - 1),
            other => other,
        };
    }
}

#[cfg(test)]
mod tests {
    use rusplay_client::PeerSecret;

    use super::*;

    fn list() -> HostList {
        HostList::new(47_800)
    }

    #[test]
    fn bare_ip_gets_the_default_port() {
        assert_eq!(
            list().parse_address("192.168.1.20").unwrap(),
            "192.168.1.20:47800".parse().unwrap()
        );
    }

    #[test]
    fn explicit_port_is_kept() {
        assert_eq!(
            list().parse_address(" 10.0.0.2:9000 ").unwrap(),
            "10.0.0.2:9000".parse().unwrap()
        );
    }

    #[test]
    fn ipv6_with_and_without_brackets() {
        assert_eq!(
            list().parse_address("::1").unwrap(),
            "[::1]:47800".parse().unwrap()
        );
        assert_eq!(
            list().parse_address("[fe80::1]:5000").unwrap(),
            "[fe80::1]:5000".parse().unwrap()
        );
    }

    #[test]
    fn garbage_is_rejected() {
        assert_eq!(
            list().parse_address("not a host"),
            Err(HostError::InvalidAddress {
                text: "not a host".to_owned()
            })
        );
    }

    #[test]
    fn adding_selects_the_new_host_and_names_it_after_the_address() {
        let mut hosts = list();

        hosts.add("", "10.0.0.2").unwrap();
        hosts.add("Living room", "10.0.0.3").unwrap();

        assert_eq!(hosts.entries()[0].name, "10.0.0.2:47800");
        assert_eq!(hosts.selected().unwrap().name, "Living room");
    }

    #[test]
    fn duplicate_addresses_are_rejected() {
        let mut hosts = list();
        hosts.add("a", "10.0.0.2").unwrap();

        assert!(matches!(
            hosts.add("b", "10.0.0.2:47800"),
            Err(HostError::Duplicate { .. })
        ));
        assert_eq!(hosts.entries().len(), 1);
    }

    #[test]
    fn removing_keeps_the_selection_pointing_at_the_same_host() {
        let mut hosts = list();
        hosts.add("a", "10.0.0.1").unwrap();
        hosts.add("b", "10.0.0.2").unwrap();
        hosts.add("c", "10.0.0.3").unwrap();

        hosts.select(2);
        hosts.remove(0);
        assert_eq!(hosts.selected().unwrap().name, "c");

        hosts.remove(1);
        assert_eq!(hosts.selected(), None);
    }

    #[test]
    fn a_paired_host_remembers_its_peer_record() {
        let mut hosts = list();
        hosts.add("a", "10.0.0.1").unwrap();
        hosts.add("b", "10.0.0.2").unwrap();
        let record = PeerRecord::new("b", PeerSecret::from_bytes([7; 32])).unwrap();

        assert!(hosts.remember_peer("10.0.0.2:47800".parse().unwrap(), record.clone()));
        assert!(!hosts.remember_peer("10.0.0.9:47800".parse().unwrap(), record.clone()));

        assert_eq!(hosts.entries()[0].peer, None);
        assert_eq!(hosts.entries()[1].peer, Some(record));
    }

    fn found(name: &str, address: &str) -> DiscoveredHost {
        DiscoveredHost {
            address: address.parse().unwrap(),
            name: name.to_owned(),
        }
    }

    #[test]
    fn discovered_hosts_are_listed_once_and_marked_online() {
        let mut hosts = list();

        hosts.merge_discovered(&[found("desk", "10.0.0.2:47800")]);
        hosts.merge_discovered(&[found("desk", "10.0.0.2:47800")]);

        assert_eq!(hosts.entries().len(), 1);
        assert_eq!(hosts.entries()[0].origin, HostOrigin::Discovered);
        assert!(hosts.entries()[0].online);
    }

    #[test]
    fn a_discovered_host_matching_a_saved_address_keeps_the_saved_name() {
        let mut hosts = list();
        hosts.add("Living room", "10.0.0.2").unwrap();

        hosts.merge_discovered(&[found("desk", "10.0.0.2:47800")]);

        assert_eq!(hosts.entries().len(), 1);
        assert_eq!(hosts.entries()[0].name, "Living room");
        assert_eq!(hosts.entries()[0].origin, HostOrigin::Added);
        assert!(hosts.entries()[0].online);
    }

    #[test]
    fn a_saved_host_that_changed_address_is_matched_by_its_name() {
        let mut hosts = list();
        hosts.add("desk", "10.0.0.2").unwrap();

        hosts.merge_discovered(&[found("desk", "10.0.0.9:47800")]);

        assert_eq!(hosts.entries().len(), 1);
        assert_eq!(
            hosts.entries()[0].address,
            "10.0.0.9:47800".parse().unwrap()
        );
        assert_eq!(
            hosts.selected().unwrap().address,
            hosts.entries()[0].address
        );
    }

    #[test]
    fn ambiguous_names_are_not_matched_by_name() {
        let mut hosts = list();
        hosts.add("desk", "10.0.0.2").unwrap();

        hosts.merge_discovered(&[
            found("desk", "10.0.0.8:47800"),
            found("desk", "10.0.0.9:47800"),
        ]);

        assert_eq!(hosts.entries().len(), 3);
        assert_eq!(
            hosts.entries()[0].address,
            "10.0.0.2:47800".parse().unwrap()
        );
        assert!(!hosts.entries()[0].online);
    }

    #[test]
    fn hosts_that_vanish_are_dropped_unless_saved() {
        let mut hosts = list();
        hosts.add("saved", "10.0.0.1").unwrap();
        hosts.merge_discovered(&[found("gone", "10.0.0.2:47800")]);
        hosts.select(1);

        hosts.merge_discovered(&[]);

        assert_eq!(hosts.entries().len(), 1);
        assert_eq!(hosts.entries()[0].name, "saved");
        assert!(!hosts.entries()[0].online);
        assert_eq!(hosts.selected(), None);
    }

    #[test]
    fn pairing_a_discovered_host_saves_it() {
        let mut hosts = list();
        hosts.merge_discovered(&[found("desk", "10.0.0.2:47800")]);
        let record = PeerRecord::new("desk", PeerSecret::from_bytes([7; 32])).unwrap();

        hosts.remember_peer("10.0.0.2:47800".parse().unwrap(), record);
        hosts.merge_discovered(&[]);

        assert_eq!(hosts.entries().len(), 1);
        assert_eq!(hosts.entries()[0].origin, HostOrigin::Added);
    }

    #[test]
    fn selecting_out_of_range_is_ignored() {
        let mut hosts = list();
        hosts.add("a", "10.0.0.1").unwrap();
        hosts.select(5);

        assert_eq!(hosts.selected_index(), Some(0));
    }
}
