use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    ops::RangeInclusive,
};

use serde::{Deserialize, Serialize};

use crate::ConfigError;

const MTU_RANGE: RangeInclusive<u16> = 576..=9000;
const MAX_FEC_OVERHEAD_PERCENT: u8 = 100;
const PIN_DIGITS_RANGE: RangeInclusive<u8> = 4..=12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkConfig {
    pub bind_address: IpAddr,
    pub port: u16,
    pub mtu: u16,
    pub fec_overhead_percent: u8,
    pub pacing: bool,
    pub jitter_buffer_ms: u16,
    pub socket_buffer_bytes: u32,
    pub reassembly_timeout_ms: u16,
    pub control_retransmit_ms: u16,
    pub control_max_retransmits: u8,
    pub discovery_port: u16,
    pub discovery_group: Ipv4Addr,
    pub pairing_pin_digits: u8,
    pub pairing_max_attempts: u8,
    pub pairing_timeout_ms: u32,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            bind_address: IpAddr::V6(Ipv6Addr::UNSPECIFIED),
            port: 47_800,
            mtu: 1_500,
            fec_overhead_percent: 10,
            pacing: true,
            jitter_buffer_ms: 0,
            socket_buffer_bytes: 8 * 1024 * 1024,
            reassembly_timeout_ms: 100,
            control_retransmit_ms: 50,
            control_max_retransmits: 8,
            discovery_port: 47_801,
            discovery_group: Ipv4Addr::new(239, 255, 47, 80),
            pairing_pin_digits: 6,
            pairing_max_attempts: 3,
            pairing_timeout_ms: 120_000,
        }
    }
}

impl NetworkConfig {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        if !MTU_RANGE.contains(&self.mtu) {
            return Err(ConfigError::MtuOutOfRange(self.mtu));
        }

        if self.fec_overhead_percent > MAX_FEC_OVERHEAD_PERCENT {
            return Err(ConfigError::FecOverheadOutOfRange(
                self.fec_overhead_percent,
            ));
        }

        if self.reassembly_timeout_ms == 0 {
            return Err(ConfigError::ZeroReassemblyTimeout);
        }

        if self.control_retransmit_ms == 0 {
            return Err(ConfigError::ZeroControlRetransmit);
        }

        if self.discovery_port == 0 {
            return Err(ConfigError::ZeroDiscoveryPort);
        }

        if !self.discovery_group.is_multicast() {
            return Err(ConfigError::DiscoveryGroupNotMulticast(
                self.discovery_group,
            ));
        }

        if !PIN_DIGITS_RANGE.contains(&self.pairing_pin_digits) {
            return Err(ConfigError::PinDigitsOutOfRange(self.pairing_pin_digits));
        }

        if self.pairing_max_attempts == 0 {
            return Err(ConfigError::ZeroPairingAttempts);
        }

        if self.pairing_timeout_ms == 0 {
            return Err(ConfigError::ZeroPairingTimeout);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_reassembly_timeout_is_rejected() {
        let config = NetworkConfig {
            reassembly_timeout_ms: 0,
            ..NetworkConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(ConfigError::ZeroReassemblyTimeout)
        ));
    }

    #[test]
    fn zero_control_retransmit_is_rejected() {
        let config = NetworkConfig {
            control_retransmit_ms: 0,
            ..NetworkConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(ConfigError::ZeroControlRetransmit)
        ));
    }

    #[test]
    fn zero_discovery_port_is_rejected() {
        let config = NetworkConfig {
            discovery_port: 0,
            ..NetworkConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(ConfigError::ZeroDiscoveryPort)
        ));
    }

    #[test]
    fn unicast_discovery_group_is_rejected() {
        let config = NetworkConfig {
            discovery_group: Ipv4Addr::new(192, 168, 1, 1),
            ..NetworkConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(ConfigError::DiscoveryGroupNotMulticast(_))
        ));
    }

    #[test]
    fn discovery_options_parse_from_toml() {
        let parsed: NetworkConfig = toml::from_str(
            r#"
            discovery_port = 5000
            discovery_group = "239.1.2.3"
            "#,
        )
        .unwrap();

        assert_eq!(parsed.discovery_port, 5000);
        assert_eq!(parsed.discovery_group, Ipv4Addr::new(239, 1, 2, 3));
        assert!(parsed.validate().is_ok());
    }

    #[test]
    fn pairing_limits_are_validated() {
        let digits = NetworkConfig {
            pairing_pin_digits: 3,
            ..NetworkConfig::default()
        };
        let attempts = NetworkConfig {
            pairing_max_attempts: 0,
            ..NetworkConfig::default()
        };
        let timeout = NetworkConfig {
            pairing_timeout_ms: 0,
            ..NetworkConfig::default()
        };

        assert!(matches!(
            digits.validate(),
            Err(ConfigError::PinDigitsOutOfRange(3))
        ));
        assert!(matches!(
            attempts.validate(),
            Err(ConfigError::ZeroPairingAttempts)
        ));
        assert!(matches!(
            timeout.validate(),
            Err(ConfigError::ZeroPairingTimeout)
        ));
    }
}
