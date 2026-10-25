use std::net::{IpAddr, SocketAddr};

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HostError {
    #[error("{text:?} is not an IP address with an optional port")]
    InvalidAddress { text: String },
    #[error("{address} is already in the list")]
    Duplicate { address: SocketAddr },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEntry {
    pub name: String,
    pub address: SocketAddr,
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

        self.entries.push(HostEntry { name, address });
        let index = self.entries.len() - 1;
        self.selected = Some(index);

        Ok(index)
    }

    pub fn select(&mut self, index: usize) {
        if index < self.entries.len() {
            self.selected = Some(index);
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
    fn selecting_out_of_range_is_ignored() {
        let mut hosts = list();
        hosts.add("a", "10.0.0.1").unwrap();
        hosts.select(5);

        assert_eq!(hosts.selected_index(), Some(0));
    }
}
