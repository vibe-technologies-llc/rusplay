#[derive(Debug, Clone)]
pub struct PadSlots<K> {
    slots: Vec<Option<K>>,
}

impl<K: PartialEq> PadSlots<K> {
    pub fn new(max_slots: u8) -> Self {
        Self {
            slots: (0..max_slots).map(|_| None).collect(),
        }
    }

    pub fn slot_of(&self, key: &K) -> Option<u8> {
        self.slots
            .iter()
            .position(|held| held.as_ref() == Some(key))
            .and_then(|index| u8::try_from(index).ok())
    }

    pub fn assign(&mut self, key: K) -> Option<u8> {
        if let Some(slot) = self.slot_of(&key) {
            return Some(slot);
        }

        let index = self.slots.iter().position(Option::is_none)?;
        self.slots[index] = Some(key);

        u8::try_from(index).ok()
    }

    pub fn release(&mut self, key: &K) -> Option<u8> {
        let slot = self.slot_of(key)?;
        self.slots[usize::from(slot)] = None;

        Some(slot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_take_the_lowest_free_slot() {
        let mut slots = PadSlots::new(3);

        assert_eq!(slots.assign("a"), Some(0));
        assert_eq!(slots.assign("b"), Some(1));
        assert_eq!(slots.release(&"a"), Some(0));
        assert_eq!(slots.assign("c"), Some(0));
    }

    #[test]
    fn a_known_device_keeps_its_slot() {
        let mut slots = PadSlots::new(2);
        slots.assign("a");
        slots.assign("b");

        assert_eq!(slots.assign("b"), Some(1));
        assert_eq!(slots.slot_of(&"a"), Some(0));
    }

    #[test]
    fn a_full_table_refuses_new_devices() {
        let mut slots = PadSlots::new(1);
        slots.assign("a");

        assert_eq!(slots.assign("b"), None);
        assert_eq!(slots.release(&"b"), None);
    }
}
