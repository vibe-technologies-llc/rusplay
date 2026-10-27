use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    ops::RangeInclusive,
};

use serde::{Deserialize, Serialize};

use crate::ConfigError;

const MTU_RANGE: RangeInclusive<u16> = 576..=9000;
const MAX_FEC_OVERHEAD_PERCENT: u8 = 100;
const PIN_DIGITS_RANGE: RangeInclusive<u8> = 4..=12;
const PACING_WINDOW_RANGE: RangeInclusive<u8> = 1..=100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkConfig {
    pub bind_address: IpAddr,
    pub port: u16,
    pub mtu: u16,
    pub fec_overhead_percent: u8,
    pub pacing: bool,
    pub pacing_window_percent: u8,
    pub jitter_buffer_ms: u16,
    pub socket_buffer_bytes: u32,
    pub reassembly_timeout_ms: u16,
    pub control_retransmit_ms: u16,
    pub control_max_retransmits: u8,
    pub discovery_port: u16,
    pub discovery_group: Ipv4Addr,
    pub discovery_scan_ms: u32,
    pub pairing_pin_digits: u8,
    pub pairing_max_attempts: u8,
    pub pairing_timeout_ms: u32,
    pub reconnect_timeout_ms: u32,
    pub handshake_timeout_ms: u32,
    pub peer_timeout_ms: u32,
    pub keepalive_interval_ms: u32,
    pub keyframe_request_interval_ms: u16,
    pub cursor_image_repeat_ms: u16,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            bind_address: IpAddr::V6(Ipv6Addr::UNSPECIFIED),
            port: 47_800,
            mtu: 1_500,
            fec_overhead_percent: 10,
            pacing: true,
            pacing_window_percent: 50,
            jitter_buffer_ms: 0,
            socket_buffer_bytes: 8 * 1024 * 1024,
            reassembly_timeout_ms: 100,
            control_retransmit_ms: 50,
            control_max_retransmits: 8,
            discovery_port: 47_801,
            discovery_group: Ipv4Addr::new(239, 255, 47, 80),
            discovery_scan_ms: 1_000,
            pairing_pin_digits: 6,
            pairing_max_attempts: 3,
            pairing_timeout_ms: 120_000,
            reconnect_timeout_ms: 5_000,
            handshake_timeout_ms: 5_000,
            peer_timeout_ms: 5_000,
            keepalive_interval_ms: 1_000,
            keyframe_request_interval_ms: 100,
            cursor_image_repeat_ms: 1_000,
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

        if !PACING_WINDOW_RANGE.contains(&self.pacing_window_percent) {
            return Err(ConfigError::PacingWindowOutOfRange(
                self.pacing_window_percent,
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

        if self.discovery_scan_ms == 0 {
            return Err(ConfigError::ZeroDiscoveryScan);
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

        if self.reconnect_timeout_ms == 0 {
            return Err(ConfigError::ZeroReconnectTimeout);
        }

        if self.handshake_timeout_ms == 0 {
            return Err(ConfigError::ZeroHandshakeTimeout);
        }

        if self.peer_timeout_ms == 0 {
            return Err(ConfigError::ZeroPeerTimeout);
        }

        if self.keepalive_interval_ms == 0 {
            return Err(ConfigError::ZeroKeepaliveInterval);
        }

        if self.keepalive_interval_ms >= self.peer_timeout_ms {
            return Err(ConfigError::KeepaliveNotBelowPeerTimeout {
                keepalive_ms: self.keepalive_interval_ms,
                peer_timeout_ms: self.peer_timeout_ms,
            });
        }

        if self.keyframe_request_interval_ms == 0 {
            return Err(ConfigError::ZeroKeyframeRequestInterval);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_session_timings_are_rejected() {
        let handshake = NetworkConfig {
            handshake_timeout_ms: 0,
            ..NetworkConfig::default()
        };
        let peer = NetworkConfig {
            peer_timeout_ms: 0,
            ..NetworkConfig::default()
        };
        let keyframe = NetworkConfig {
            keyframe_request_interval_ms: 0,
            ..NetworkConfig::default()
        };

        assert!(matches!(
            handshake.validate(),
            Err(ConfigError::ZeroHandshakeTimeout)
        ));
        assert!(matches!(peer.validate(), Err(ConfigError::ZeroPeerTimeout)));
        assert!(matches!(
            keyframe.validate(),
            Err(ConfigError::ZeroKeyframeRequestInterval)
        ));
    }

    #[test]
    fn keepalive_must_fire_within_the_peer_timeout() {
        let zero = NetworkConfig {
            keepalive_interval_ms: 0,
            ..NetworkConfig::default()
        };
        let too_slow = NetworkConfig {
            keepalive_interval_ms: 5_000,
            peer_timeout_ms: 5_000,
            ..NetworkConfig::default()
        };

        assert!(matches!(
            zero.validate(),
            Err(ConfigError::ZeroKeepaliveInterval)
        ));
        assert!(matches!(
            too_slow.validate(),
            Err(ConfigError::KeepaliveNotBelowPeerTimeout {
                keepalive_ms: 5_000,
                peer_timeout_ms: 5_000
            })
        ));
    }

    #[test]
    fn pacing_window_must_be_a_percentage_of_the_frame_interval() {
        for percent in [0, 101] {
            let config = NetworkConfig {
                pacing_window_percent: percent,
                ..NetworkConfig::default()
            };

            assert!(matches!(
                config.validate(),
                Err(ConfigError::PacingWindowOutOfRange(found)) if found == percent
            ));
        }
    }

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
    fn zero_discovery_scan_is_rejected() {
        let config = NetworkConfig {
            discovery_scan_ms: 0,
            ..NetworkConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(ConfigError::ZeroDiscoveryScan)
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

    #[test]
    fn zero_reconnect_timeout_is_rejected() {
        let config = NetworkConfig {
            reconnect_timeout_ms: 0,
            ..NetworkConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(ConfigError::ZeroReconnectTimeout)
        ));
    }

    #[test]
    fn cursor_image_repeats_every_second_by_default() {
        assert_eq!(NetworkConfig::default().cursor_image_repeat_ms, 1_000);
    }
}
