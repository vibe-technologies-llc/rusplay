use std::net::{IpAddr, SocketAddr};

use rusplay_client::{
    DiscoveredHost, HostId as AnnouncedHostId, MAX_PEER_NAME_BYTES, PeerRecord, SavedHost,
};
use rusplay_config::HostProfile;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HostError {
    #[error("{text:?} is not an IP address with an optional port")]
    InvalidAddress { text: String },
    #[error("{address} is already in the list")]
    Duplicate { address: SocketAddr },
    #[error("a host name of {len} bytes exceeds the limit of {max}")]
    NameTooLong { len: usize, max: usize },
    #[error("a host name must not contain control characters")]
    NameControlCharacter,
    #[error("no host in the host book is named or addressed {target:?}")]
    UnknownHost { target: String },
    #[error("{count} hosts in the host book are named {target:?}")]
    AmbiguousHost { target: String, count: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostOrigin {
    Added,
    Discovered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostId(u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEntry {
    pub id: HostId,
    pub name: String,
    pub address: SocketAddr,
    pub host_id: Option<AnnouncedHostId>,
    pub peer: Option<PeerRecord>,
    pub profile: HostProfile,
    pub origin: HostOrigin,
    pub online: bool,
    pub busy: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostList {
    default_port: u16,
    entries: Vec<HostEntry>,
    selected: Option<usize>,
    next_id: u64,
}

impl HostList {
    pub const fn new(default_port: u16) -> Self {
        Self {
            default_port,
            entries: Vec::new(),
            selected: None,
            next_id: 0,
        }
    }

    fn allocate_id(&mut self) -> HostId {
        let id = HostId(self.next_id);
        self.next_id += 1;

        id
    }

    pub fn from_saved(default_port: u16, saved: &[SavedHost]) -> Self {
        let mut list = Self::new(default_port);

        list.entries = saved
            .iter()
            .map(|host| HostEntry {
                id: list.allocate_id(),
                name: host.name.clone(),
                address: host.address,
                host_id: host.host_id,
                peer: host.peer.clone(),
                profile: host.profile.clone(),
                origin: HostOrigin::Added,
                online: false,
                busy: false,
            })
            .collect();

        list
    }

    pub fn saved_hosts(&self) -> Vec<SavedHost> {
        self.entries
            .iter()
            .filter(|entry| entry.origin == HostOrigin::Added)
            .map(|entry| SavedHost {
                name: entry.name.clone(),
                address: entry.address,
                host_id: entry.host_id,
                peer: entry.peer.clone(),
                profile: entry.profile.clone(),
            })
            .collect()
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

    pub fn find(&self, target: &str) -> Result<usize, HostError> {
        let target = target.trim();
        let named: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.name.eq_ignore_ascii_case(target))
            .map(|(index, _)| index)
            .collect();

        match named.as_slice() {
            [index] => return Ok(*index),
            [] => {}
            many => {
                return Err(HostError::AmbiguousHost {
                    target: target.to_owned(),
                    count: many.len(),
                });
            }
        }

        let address = self
            .parse_address(target)
            .map_err(|_| HostError::UnknownHost {
                target: target.to_owned(),
            })?;

        self.entries
            .iter()
            .position(|entry| entry.address == address)
            .ok_or_else(|| HostError::UnknownHost {
                target: target.to_owned(),
            })
    }

    pub fn add(&mut self, name: &str, address_text: &str) -> Result<usize, HostError> {
        let address = self.parse_address(address_text)?;

        if self.entries.iter().any(|entry| entry.address == address) {
            return Err(HostError::Duplicate { address });
        }

        let name = display_name(name, address)?;

        let id = self.allocate_id();
        self.entries.push(HostEntry {
            id,
            name,
            address,
            host_id: None,
            peer: None,
            profile: HostProfile::default(),
            origin: HostOrigin::Added,
            online: false,
            busy: false,
        });
        let index = self.entries.len() - 1;
        self.selected = Some(index);

        Ok(index)
    }

    pub fn rename(&mut self, index: usize, name: &str) -> Result<(), HostError> {
        let Some(address) = self.entries.get(index).map(|entry| entry.address) else {
            return Ok(());
        };
        let name = display_name(name, address)?;

        let entry = &mut self.entries[index];
        entry.name = name;
        entry.origin = HostOrigin::Added;
        Ok(())
    }

    pub fn set_address(&mut self, index: usize, address_text: &str) -> Result<(), HostError> {
        if index >= self.entries.len() {
            return Ok(());
        }
        let address = self.parse_address(address_text)?;

        if self
            .entries
            .iter()
            .enumerate()
            .any(|(other, entry)| other != index && entry.address == address)
        {
            return Err(HostError::Duplicate { address });
        }

        let entry = &mut self.entries[index];
        entry.address = address;
        entry.host_id = None;
        entry.origin = HostOrigin::Added;
        entry.online = false;
        entry.busy = false;
        Ok(())
    }

    pub fn set_profile(&mut self, index: usize, profile: HostProfile) {
        if let Some(entry) = self.entries.get_mut(index) {
            entry.profile = profile;
            entry.origin = HostOrigin::Added;
        }
    }

    pub fn forget_pairing(&mut self, index: usize) -> bool {
        self.entries
            .get_mut(index)
            .is_some_and(|entry| entry.peer.take().is_some())
    }

    pub fn select(&mut self, index: usize) {
        if index < self.entries.len() {
            self.selected = Some(index);
        }
    }

    pub fn remember_peer(&mut self, id: HostId, peer: PeerRecord) -> bool {
        match self.entries.iter_mut().find(|entry| entry.id == id) {
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
            entry.busy = false;
        }

        let selected = self.selected().map(|entry| entry.id);

        let mut unmatched = Vec::new();
        for host in found {
            match announced_id(host).and_then(|id| self.position_by_host_id(id)) {
                Some(index) => self.mark_seen(index, host, true),
                None => unmatched.push(host),
            }
        }

        let mut unplaced = Vec::new();
        for host in unmatched {
            match self.position_by_address(host) {
                Some(index) => self.mark_seen(index, host, false),
                None => unplaced.push(host),
            }
        }

        let mut new_hosts = Vec::new();
        for host in &unplaced {
            if self.move_saved_host(host, &unplaced) {
                continue;
            }
            let id = self.allocate_id();
            new_hosts.push(HostEntry {
                id,
                name: announced_name(host),
                address: host.address,
                host_id: announced_id(host),
                peer: None,
                profile: HostProfile::default(),
                origin: HostOrigin::Discovered,
                online: true,
                busy: host.busy,
            });
        }

        self.entries
            .retain(|entry| entry.online || entry.origin == HostOrigin::Added);
        self.entries.extend(new_hosts);
        self.selected =
            selected.and_then(|id| self.entries.iter().position(|entry| entry.id == id));
    }

    fn position_by_host_id(&self, id: AnnouncedHostId) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.host_id == Some(id))
    }

    fn position_by_address(&self, host: &DiscoveredHost) -> Option<usize> {
        let announced = announced_id(host);

        self.entries.iter().position(|entry| {
            entry.address == host.address && (entry.host_id.is_none() || announced.is_none())
        })
    }

    fn address_is_taken(&self, address: SocketAddr, except: usize) -> bool {
        self.entries
            .iter()
            .enumerate()
            .any(|(index, entry)| index != except && entry.address == address)
    }

    fn mark_seen(&mut self, index: usize, host: &DiscoveredHost, follow_address: bool) {
        let moves = follow_address
            && self.entries[index].address != host.address
            && !self.address_is_taken(host.address, index);

        let entry = &mut self.entries[index];
        entry.online = true;
        entry.busy = host.busy;
        if entry.host_id.is_none() {
            entry.host_id = announced_id(host);
        }
        if moves {
            entry.address = host.address;
        }
        if entry.origin == HostOrigin::Discovered {
            entry.name = announced_name(host);
        }
    }

    fn move_saved_host(&mut self, host: &DiscoveredHost, unplaced: &[&DiscoveredHost]) -> bool {
        let announced_once = unplaced
            .iter()
            .filter(|other| other.name == host.name)
            .count()
            == 1;
        if !announced_once {
            return false;
        }

        let candidates: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                entry.origin == HostOrigin::Added
                    && !entry.online
                    && entry.host_id.is_none()
                    && entry.name == host.name
            })
            .map(|(index, _)| index)
            .collect();

        match candidates.as_slice() {
            [index] if !self.address_is_taken(host.address, *index) => {
                let entry = &mut self.entries[*index];
                entry.address = host.address;
                entry.host_id = announced_id(host);
                entry.online = true;
                entry.busy = host.busy;
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

fn display_name(name: &str, address: SocketAddr) -> Result<String, HostError> {
    let trimmed = name.trim();

    if trimmed.is_empty() {
        return Ok(address.to_string());
    }

    if trimmed.len() > MAX_PEER_NAME_BYTES {
        return Err(HostError::NameTooLong {
            len: trimmed.len(),
            max: MAX_PEER_NAME_BYTES,
        });
    }

    if trimmed.chars().any(char::is_control) {
        return Err(HostError::NameControlCharacter);
    }

    Ok(trimmed.to_owned())
}

fn announced_id(host: &DiscoveredHost) -> Option<AnnouncedHostId> {
    (!host.id.is_none()).then_some(host.id)
}

fn announced_name(host: &DiscoveredHost) -> String {
    let printable: String = host
        .name
        .chars()
        .filter(|character| !character.is_control())
        .collect();

    match printable.trim() {
        "" => host.address.to_string(),
        name => name.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use rusplay_client::PeerSecret;
    use rusplay_config::PresentMode;

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

        let known = hosts.entries()[1].id;
        let unknown = HostId(99);

        assert!(hosts.remember_peer(known, record.clone()));
        assert!(!hosts.remember_peer(unknown, record.clone()));

        assert_eq!(hosts.entries()[0].peer, None);
        assert_eq!(hosts.entries()[1].peer, Some(record));
    }

    fn found(name: &str, address: &str) -> DiscoveredHost {
        DiscoveredHost {
            address: address.parse().unwrap(),
            name: name.to_owned(),
            busy: false,
            id: AnnouncedHostId::NONE,
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
    fn a_pairing_is_remembered_after_the_host_changed_address() {
        let mut hosts = list();
        hosts.add("desk", "10.0.0.2").unwrap();
        let id = hosts.entries()[0].id;
        let record = PeerRecord::new("desk", PeerSecret::from_bytes([7; 32])).unwrap();

        hosts.merge_discovered(&[found("desk", "10.0.0.9:47800")]);

        assert!(hosts.remember_peer(id, record.clone()));
        assert_eq!(hosts.entries()[0].peer, Some(record));
    }

    #[test]
    fn a_pairing_for_a_removed_host_is_reported_as_missed() {
        let mut hosts = list();
        hosts.add("desk", "10.0.0.2").unwrap();
        let id = hosts.entries()[0].id;
        let record = PeerRecord::new("desk", PeerSecret::from_bytes([7; 32])).unwrap();

        hosts.remove(0);

        assert!(!hosts.remember_peer(id, record));
    }

    #[test]
    fn ids_are_not_reused_after_a_removal() {
        let mut hosts = list();
        hosts.add("a", "10.0.0.1").unwrap();
        let first = hosts.entries()[0].id;

        hosts.remove(0);
        hosts.add("b", "10.0.0.2").unwrap();

        assert_ne!(hosts.entries()[0].id, first);
    }

    #[test]
    fn a_host_is_found_by_name_or_address() {
        let mut hosts = list();
        hosts.add("Desk", "10.0.0.2").unwrap();
        hosts.add("lap", "10.0.0.3:9000").unwrap();

        assert_eq!(hosts.find("desk"), Ok(0));
        assert_eq!(hosts.find("  LAP "), Ok(1));
        assert_eq!(hosts.find("10.0.0.2"), Ok(0));
        assert_eq!(hosts.find("10.0.0.3:9000"), Ok(1));
        assert!(matches!(
            hosts.find("10.0.0.3"),
            Err(HostError::UnknownHost { .. })
        ));
        assert!(matches!(
            hosts.find("nothing"),
            Err(HostError::UnknownHost { .. })
        ));
    }

    #[test]
    fn a_name_shared_by_several_hosts_is_ambiguous() {
        let mut hosts = list();
        hosts.add("desk", "10.0.0.2").unwrap();
        hosts.add("Desk", "10.0.0.3").unwrap();

        assert_eq!(
            hosts.find("desk"),
            Err(HostError::AmbiguousHost {
                target: "desk".to_owned(),
                count: 2
            })
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

        let id = hosts.entries()[0].id;
        hosts.remember_peer(id, record);
        hosts.merge_discovered(&[]);

        assert_eq!(hosts.entries().len(), 1);
        assert_eq!(hosts.entries()[0].origin, HostOrigin::Added);
    }

    #[test]
    fn renaming_validates_and_saves_the_host() {
        let mut hosts = list();
        hosts.merge_discovered(&[found("desk", "10.0.0.2:47800")]);

        assert_eq!(
            hosts.rename(0, "bad\nname"),
            Err(HostError::NameControlCharacter)
        );
        assert!(matches!(
            hosts.rename(0, &"x".repeat(MAX_PEER_NAME_BYTES + 1)),
            Err(HostError::NameTooLong { .. })
        ));
        hosts.rename(0, "  Den  ").unwrap();
        hosts.merge_discovered(&[]);

        assert_eq!(hosts.entries()[0].name, "Den");
        assert_eq!(hosts.saved_hosts().len(), 1);
    }

    #[test]
    fn editing_the_address_keeps_the_pairing_and_rejects_duplicates() {
        let mut hosts = list();
        hosts.add("a", "10.0.0.1").unwrap();
        hosts.add("b", "10.0.0.2").unwrap();
        let record = PeerRecord::new("a", PeerSecret::from_bytes([7; 32])).unwrap();
        let id = hosts.entries()[0].id;
        hosts.remember_peer(id, record.clone());

        assert!(matches!(
            hosts.set_address(0, "10.0.0.2"),
            Err(HostError::Duplicate { .. })
        ));
        assert!(matches!(
            hosts.set_address(0, "nope"),
            Err(HostError::InvalidAddress { .. })
        ));
        hosts.set_address(0, "10.0.0.9:9000").unwrap();

        assert_eq!(hosts.entries()[0].address, "10.0.0.9:9000".parse().unwrap());
        assert_eq!(hosts.entries()[0].peer, Some(record));
    }

    #[test]
    fn forgetting_a_pairing_keeps_the_host() {
        let mut hosts = list();
        hosts.add("a", "10.0.0.1").unwrap();
        let record = PeerRecord::new("a", PeerSecret::from_bytes([7; 32])).unwrap();
        let id = hosts.entries()[0].id;
        hosts.remember_peer(id, record);

        assert!(hosts.forget_pairing(0));
        assert!(!hosts.forget_pairing(0));
        assert!(!hosts.forget_pairing(4));

        assert_eq!(hosts.entries().len(), 1);
        assert_eq!(hosts.entries()[0].peer, None);
    }

    #[test]
    fn saved_hosts_round_trip_and_leave_out_discovered_ones() {
        let mut hosts = list();
        hosts.add("a", "10.0.0.1").unwrap();
        hosts.merge_discovered(&[found("seen", "10.0.0.5:47800")]);

        let saved = hosts.saved_hosts();
        let restored = HostList::from_saved(47_800, &saved);

        assert_eq!(saved.len(), 1);
        assert_eq!(restored.entries().len(), 1);
        assert_eq!(restored.entries()[0].name, "a");
        assert_eq!(restored.selected(), None);
    }

    #[test]
    fn announced_names_lose_control_characters() {
        let mut hosts = list();

        hosts.merge_discovered(&[
            found("de\nsk", "10.0.0.2:47800"),
            found("\u{7}", "10.0.0.3:47800"),
        ]);

        assert_eq!(hosts.entries()[0].name, "desk");
        assert_eq!(hosts.entries()[1].name, "10.0.0.3:47800");
    }

    #[test]
    fn selecting_out_of_range_is_ignored() {
        let mut hosts = list();
        hosts.add("a", "10.0.0.1").unwrap();
        hosts.select(5);

        assert_eq!(hosts.selected_index(), Some(0));
    }

    #[test]
    fn a_profile_is_saved_with_its_host_and_survives_the_list_round_trip() {
        let mut hosts = list();
        hosts.add("a", "10.0.0.1").unwrap();
        hosts.add("b", "10.0.0.2").unwrap();
        let profile = HostProfile {
            present_mode: Some(PresentMode::Fifo),
            ..HostProfile::default()
        };

        hosts.set_profile(1, profile.clone());
        let saved = hosts.saved_hosts();
        let restored = HostList::from_saved(47_800, &saved);

        assert!(saved[0].profile.is_empty());
        assert_eq!(saved[1].profile, profile);
        assert_eq!(restored.entries()[1].profile, profile);
    }

    #[test]
    fn a_profile_on_a_discovered_host_saves_it() {
        let mut hosts = list();
        hosts.merge_discovered(&[found("den", "10.0.0.5:47800")]);
        assert_eq!(hosts.saved_hosts().len(), 0);

        hosts.set_profile(
            0,
            HostProfile {
                keyboard: Some(false),
                ..HostProfile::default()
            },
        );

        assert_eq!(hosts.saved_hosts().len(), 1);
    }

    fn identified(name: &str, address: &str, byte: u8) -> DiscoveredHost {
        DiscoveredHost {
            id: AnnouncedHostId([byte; AnnouncedHostId::LEN]),
            ..found(name, address)
        }
    }

    fn saved_with_id(name: &str, address: &str, byte: u8) -> SavedHost {
        SavedHost {
            name: name.to_owned(),
            address: address.parse().unwrap(),
            host_id: Some(AnnouncedHostId([byte; AnnouncedHostId::LEN])),
            peer: None,
            profile: HostProfile::default(),
        }
    }

    #[test]
    fn the_busy_flag_follows_the_latest_scan() {
        let mut hosts = list();
        hosts.add("saved", "10.0.0.1").unwrap();
        let busy = DiscoveredHost {
            busy: true,
            ..found("saved", "10.0.0.1:47800")
        };
        let busy_new = DiscoveredHost {
            busy: true,
            ..found("desk", "10.0.0.2:47800")
        };

        hosts.merge_discovered(&[busy, busy_new]);
        assert!(hosts.entries()[0].busy);
        assert!(hosts.entries()[1].busy);

        hosts.merge_discovered(&[found("saved", "10.0.0.1:47800")]);
        assert!(!hosts.entries()[0].busy);
        assert_eq!(hosts.entries().len(), 1);
    }

    #[test]
    fn a_host_that_was_not_seen_is_not_busy() {
        let mut hosts = list();
        hosts.add("saved", "10.0.0.1").unwrap();
        hosts.merge_discovered(&[DiscoveredHost {
            busy: true,
            ..found("saved", "10.0.0.1:47800")
        }]);

        hosts.merge_discovered(&[]);

        assert!(!hosts.entries()[0].online);
        assert!(!hosts.entries()[0].busy);
    }

    #[test]
    fn a_saved_host_is_matched_by_id_after_a_rename_and_an_address_change() {
        let mut hosts = HostList::from_saved(47_800, &[saved_with_id("Den", "10.0.0.2:47800", 7)]);

        hosts.merge_discovered(&[identified("desk", "10.0.0.9:47800", 7)]);

        assert_eq!(hosts.entries().len(), 1);
        assert_eq!(hosts.entries()[0].name, "Den");
        assert_eq!(hosts.entries()[0].origin, HostOrigin::Added);
        assert_eq!(
            hosts.entries()[0].address,
            "10.0.0.9:47800".parse().unwrap()
        );
        assert!(hosts.entries()[0].online);
    }

    #[test]
    fn saved_hosts_with_the_same_name_are_told_apart_by_id() {
        let mut hosts = HostList::from_saved(
            47_800,
            &[
                saved_with_id("desk", "10.0.0.2:47800", 1),
                saved_with_id("desk", "10.0.0.3:47800", 2),
            ],
        );

        hosts.merge_discovered(&[
            identified("desk", "10.0.0.8:47800", 2),
            identified("desk", "10.0.0.7:47800", 1),
        ]);

        assert_eq!(hosts.entries().len(), 2);
        assert_eq!(
            hosts.entries()[0].address,
            "10.0.0.7:47800".parse().unwrap()
        );
        assert_eq!(
            hosts.entries()[1].address,
            "10.0.0.8:47800".parse().unwrap()
        );
        assert!(hosts.entries().iter().all(|entry| entry.online));
    }

    #[test]
    fn the_selection_follows_a_host_matched_by_id() {
        let mut hosts = HostList::from_saved(47_800, &[saved_with_id("Den", "10.0.0.2:47800", 7)]);
        hosts.select(0);

        hosts.merge_discovered(&[identified("desk", "10.0.0.9:47800", 7)]);

        assert_eq!(
            hosts.selected().unwrap().address,
            "10.0.0.9:47800".parse().unwrap()
        );
    }

    #[test]
    fn a_saved_host_learns_the_id_announced_at_its_address() {
        let mut hosts = list();
        hosts.add("Den", "10.0.0.2").unwrap();
        let before = hosts.saved_hosts();

        hosts.merge_discovered(&[identified("desk", "10.0.0.2:47800", 7)]);

        assert_eq!(
            hosts.entries()[0].host_id,
            Some(AnnouncedHostId([7; AnnouncedHostId::LEN]))
        );
        assert_ne!(hosts.saved_hosts(), before);
        assert_eq!(hosts.saved_hosts()[0].host_id, hosts.entries()[0].host_id);
    }

    #[test]
    fn a_saved_host_without_an_id_moves_by_name_and_learns_the_id() {
        let mut hosts = list();
        hosts.add("desk", "10.0.0.2").unwrap();

        hosts.merge_discovered(&[identified("desk", "10.0.0.9:47800", 7)]);

        assert_eq!(hosts.entries().len(), 1);
        assert_eq!(
            hosts.entries()[0].address,
            "10.0.0.9:47800".parse().unwrap()
        );
        assert_eq!(
            hosts.entries()[0].host_id,
            Some(AnnouncedHostId([7; AnnouncedHostId::LEN]))
        );
    }

    #[test]
    fn a_saved_host_with_an_id_is_not_moved_by_name() {
        let mut hosts = HostList::from_saved(47_800, &[saved_with_id("desk", "10.0.0.2:47800", 1)]);

        hosts.merge_discovered(&[identified("desk", "10.0.0.9:47800", 2)]);

        assert_eq!(hosts.entries().len(), 2);
        assert_eq!(
            hosts.entries()[0].address,
            "10.0.0.2:47800".parse().unwrap()
        );
        assert!(!hosts.entries()[0].online);
        assert_eq!(hosts.entries()[1].origin, HostOrigin::Discovered);
    }

    #[test]
    fn another_host_at_a_saved_address_is_listed_apart() {
        let mut hosts = HostList::from_saved(47_800, &[saved_with_id("desk", "10.0.0.2:47800", 1)]);

        hosts.merge_discovered(&[identified("other", "10.0.0.2:47800", 2)]);

        assert_eq!(hosts.entries().len(), 2);
        assert!(!hosts.entries()[0].online);
        assert_eq!(hosts.entries()[1].name, "other");
        assert!(hosts.entries()[1].online);
    }

    #[test]
    fn a_host_matched_by_id_does_not_take_an_address_another_entry_holds() {
        let mut hosts = HostList::from_saved(
            47_800,
            &[
                saved_with_id("a", "10.0.0.2:47800", 1),
                saved_with_id("b", "10.0.0.3:47800", 2),
            ],
        );

        hosts.merge_discovered(&[identified("a", "10.0.0.3:47800", 1)]);

        assert_eq!(hosts.entries().len(), 2);
        assert_eq!(
            hosts.entries()[0].address,
            "10.0.0.2:47800".parse().unwrap()
        );
        assert!(hosts.entries()[0].online);
        assert!(!hosts.entries()[1].online);
    }

    #[test]
    fn a_discovered_host_carries_its_announced_id() {
        let mut hosts = list();

        hosts.merge_discovered(&[
            identified("desk", "10.0.0.2:47800", 7),
            found("old", "10.0.0.3:47800"),
        ]);

        assert_eq!(
            hosts.entries()[0].host_id,
            Some(AnnouncedHostId([7; AnnouncedHostId::LEN]))
        );
        assert_eq!(hosts.entries()[1].host_id, None);
    }

    #[test]
    fn editing_the_address_forgets_the_announced_id() {
        let mut hosts = HostList::from_saved(47_800, &[saved_with_id("Den", "10.0.0.2:47800", 7)]);

        hosts.set_address(0, "10.0.0.5").unwrap();

        assert_eq!(hosts.entries()[0].host_id, None);
    }

    #[test]
    fn saved_hosts_round_trip_their_id() {
        let saved = vec![
            saved_with_id("Den", "10.0.0.2:47800", 7),
            SavedHost {
                host_id: None,
                ..saved_with_id("old", "10.0.0.3:47800", 1)
            },
        ];

        let hosts = HostList::from_saved(47_800, &saved);

        assert_eq!(hosts.saved_hosts(), saved);
    }
}
