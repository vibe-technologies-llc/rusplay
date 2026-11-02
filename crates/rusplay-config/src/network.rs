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
const NICE_RANGE: RangeInclusive<i8> = -20..=19;
const RTKIT_PRIORITY_MAX: u8 = 99;
const KBPS_PER_MBPS: u32 = 1_000;
const BACKOFF_PERCENT_RANGE: RangeInclusive<u8> = 1..=90;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkConfig {
    pub bind_address: IpAddr,
    pub port: u16,
    pub mtu: u16,
    pub fec_overhead_percent: u8,
    pub pacing: bool,
    pub pacing_window_percent: u8,
    pub pacing_max_mbps: u32,
    pub frame_thread_nice: i8,
    pub rtkit_priority: u8,
    pub send_timer_slack_us: u32,
    pub socket_buffer_bytes: u32,
    pub reassembly_timeout_ms: u16,
    pub reassembly_max_pending_frames: u8,
    pub reassembly_max_fragments: u16,
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
    pub stats_log_interval_ms: u32,
    pub stats_json: bool,
    pub adaptive_bitrate: bool,
    pub adaptive_bitrate_floor_kbps: u32,
    pub adaptive_bitrate_step_kbps: u32,
    pub adaptive_bitrate_window_ms: u32,
    pub adaptive_bitrate_backoff_percent: u8,
    pub bitrate_hint_min_kbps: u32,
    pub bitrate_hint_max_kbps: u32,
    pub clock_sync_interval_ms: u32,
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
            pacing_max_mbps: 0,
            frame_thread_nice: -5,
            rtkit_priority: 0,
            send_timer_slack_us: 1,
            socket_buffer_bytes: 8 * 1024 * 1024,
            reassembly_timeout_ms: 100,
            reassembly_max_pending_frames: 8,
            reassembly_max_fragments: u16::MAX,
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
            stats_log_interval_ms: 10_000,
            stats_json: false,
            adaptive_bitrate: true,
            adaptive_bitrate_floor_kbps: 10_000,
            adaptive_bitrate_step_kbps: 2_000,
            adaptive_bitrate_window_ms: 1_000,
            adaptive_bitrate_backoff_percent: 25,
            bitrate_hint_min_kbps: 2_000,
            bitrate_hint_max_kbps: 200_000,
            clock_sync_interval_ms: 1_000,
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

        if !NICE_RANGE.contains(&self.frame_thread_nice) {
            return Err(ConfigError::FrameThreadNiceOutOfRange(
                self.frame_thread_nice,
            ));
        }

        if self.rtkit_priority > RTKIT_PRIORITY_MAX {
            return Err(ConfigError::RtkitPriorityOutOfRange(self.rtkit_priority));
        }

        if self.reassembly_timeout_ms == 0 {
            return Err(ConfigError::ZeroReassemblyTimeout);
        }

        if self.reassembly_max_pending_frames == 0 {
            return Err(ConfigError::ZeroReassemblyPendingFrames);
        }

        if self.reassembly_max_fragments == 0 {
            return Err(ConfigError::ZeroReassemblyFragments);
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

        self.validate_bitrate_adaptation()
    }

    fn validate_bitrate_adaptation(&self) -> Result<(), ConfigError> {
        if self.adaptive_bitrate_floor_kbps == 0 {
            return Err(ConfigError::ZeroAdaptiveBitrateFloor);
        }

        if self.adaptive_bitrate_step_kbps == 0 {
            return Err(ConfigError::ZeroAdaptiveBitrateStep);
        }

        if self.adaptive_bitrate_window_ms == 0 {
            return Err(ConfigError::ZeroAdaptiveBitrateWindow);
        }

        if !BACKOFF_PERCENT_RANGE.contains(&self.adaptive_bitrate_backoff_percent) {
            return Err(ConfigError::AdaptiveBitrateBackoffOutOfRange(
                self.adaptive_bitrate_backoff_percent,
            ));
        }

        if self.bitrate_hint_min_kbps == 0 {
            return Err(ConfigError::ZeroBitrateHintMin);
        }

        if self.bitrate_hint_min_kbps > self.bitrate_hint_max_kbps {
            return Err(ConfigError::BitrateHintRangeInverted {
                min_kbps: self.bitrate_hint_min_kbps,
                max_kbps: self.bitrate_hint_max_kbps,
            });
        }

        Ok(())
    }

    pub fn pacing_max_kbps(&self) -> Option<u32> {
        (self.pacing_max_mbps > 0).then(|| self.pacing_max_mbps.saturating_mul(KBPS_PER_MBPS))
    }

    pub fn bitrate_hint_range(&self) -> RangeInclusive<u32> {
        self.bitrate_hint_min_kbps..=self.bitrate_hint_max_kbps
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
    fn clock_sync_defaults_to_one_second_and_zero_turns_it_off() {
        let off = NetworkConfig {
            clock_sync_interval_ms: 0,
            ..NetworkConfig::default()
        };

        assert_eq!(NetworkConfig::default().clock_sync_interval_ms, 1_000);
        assert!(off.validate().is_ok());
    }

    #[test]
    fn stats_log_interval_defaults_to_ten_seconds_and_zero_is_valid() {
        let silent = NetworkConfig {
            stats_log_interval_ms: 0,
            ..NetworkConfig::default()
        };

        assert_eq!(NetworkConfig::default().stats_log_interval_ms, 10_000);
        assert!(silent.validate().is_ok());
    }

    #[test]
    fn json_statistics_are_off_by_default_and_parse_from_toml() {
        let parsed: NetworkConfig = toml::from_str("stats_json = true").unwrap();

        assert!(!NetworkConfig::default().stats_json);
        assert!(parsed.stats_json);
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
    fn the_pacing_rate_cap_is_off_at_zero_and_converts_to_kilobits() {
        let capped = NetworkConfig {
            pacing_max_mbps: 940,
            ..NetworkConfig::default()
        };

        assert_eq!(NetworkConfig::default().pacing_max_kbps(), None);
        assert_eq!(capped.pacing_max_kbps(), Some(940_000));
        assert!(capped.validate().is_ok());
    }

    #[test]
    fn the_frame_thread_nice_value_must_be_a_valid_nice_level() {
        for nice in [-21, 20] {
            let config = NetworkConfig {
                frame_thread_nice: nice,
                ..NetworkConfig::default()
            };

            assert!(matches!(
                config.validate(),
                Err(ConfigError::FrameThreadNiceOutOfRange(found)) if found == nice
            ));
        }
        for nice in [-20, 0, 19] {
            let config = NetworkConfig {
                frame_thread_nice: nice,
                ..NetworkConfig::default()
            };

            assert!(config.validate().is_ok());
        }
    }

    #[test]
    fn the_rtkit_priority_is_off_by_default_and_capped_at_the_scheduler_maximum() {
        assert_eq!(NetworkConfig::default().rtkit_priority, 0);

        for priority in [0, 1, 99] {
            let config = NetworkConfig {
                rtkit_priority: priority,
                ..NetworkConfig::default()
            };

            assert!(config.validate().is_ok());
        }

        let config = NetworkConfig {
            rtkit_priority: 100,
            ..NetworkConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(ConfigError::RtkitPriorityOutOfRange(100))
        ));
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
    fn zero_reassembly_pending_frames_is_rejected() {
        let config = NetworkConfig {
            reassembly_max_pending_frames: 0,
            ..NetworkConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(ConfigError::ZeroReassemblyPendingFrames)
        ));
    }

    #[test]
    fn zero_reassembly_fragments_is_rejected() {
        let config = NetworkConfig {
            reassembly_max_fragments: 0,
            ..NetworkConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(ConfigError::ZeroReassemblyFragments)
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
    fn adaptive_bitrate_settings_are_validated() {
        let floor = NetworkConfig {
            adaptive_bitrate_floor_kbps: 0,
            ..NetworkConfig::default()
        };
        let step = NetworkConfig {
            adaptive_bitrate_step_kbps: 0,
            ..NetworkConfig::default()
        };
        let window = NetworkConfig {
            adaptive_bitrate_window_ms: 0,
            ..NetworkConfig::default()
        };

        assert!(matches!(
            floor.validate(),
            Err(ConfigError::ZeroAdaptiveBitrateFloor)
        ));
        assert!(matches!(
            step.validate(),
            Err(ConfigError::ZeroAdaptiveBitrateStep)
        ));
        assert!(matches!(
            window.validate(),
            Err(ConfigError::ZeroAdaptiveBitrateWindow)
        ));
        for percent in [0, 91] {
            let backoff = NetworkConfig {
                adaptive_bitrate_backoff_percent: percent,
                ..NetworkConfig::default()
            };

            assert!(matches!(
                backoff.validate(),
                Err(ConfigError::AdaptiveBitrateBackoffOutOfRange(found)) if found == percent
            ));
        }
    }

    #[test]
    fn the_bitrate_hint_range_must_be_ordered_and_above_zero() {
        let zero = NetworkConfig {
            bitrate_hint_min_kbps: 0,
            ..NetworkConfig::default()
        };
        let inverted = NetworkConfig {
            bitrate_hint_min_kbps: 50_000,
            bitrate_hint_max_kbps: 40_000,
            ..NetworkConfig::default()
        };
        let single = NetworkConfig {
            bitrate_hint_min_kbps: 40_000,
            bitrate_hint_max_kbps: 40_000,
            ..NetworkConfig::default()
        };

        assert!(matches!(
            zero.validate(),
            Err(ConfigError::ZeroBitrateHintMin)
        ));
        assert!(matches!(
            inverted.validate(),
            Err(ConfigError::BitrateHintRangeInverted {
                min_kbps: 50_000,
                max_kbps: 40_000
            })
        ));
        assert!(single.validate().is_ok());
        assert_eq!(single.bitrate_hint_range(), 40_000..=40_000);
    }

    #[test]
    fn cursor_image_repeats_every_second_by_default() {
        assert_eq!(NetworkConfig::default().cursor_image_repeat_ms, 1_000);
    }
}
