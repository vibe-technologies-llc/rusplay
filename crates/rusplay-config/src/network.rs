use std::{
    net::{IpAddr, Ipv6Addr},
    ops::RangeInclusive,
};

use serde::{Deserialize, Serialize};

use crate::ConfigError;

const MTU_RANGE: RangeInclusive<u16> = 576..=9000;
const MAX_FEC_OVERHEAD_PERCENT: u8 = 100;

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

        Ok(())
    }
}
