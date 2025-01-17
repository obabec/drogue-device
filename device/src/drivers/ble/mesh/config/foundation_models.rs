use embassy::time::Duration;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct FoundationModels {
    pub(crate) configuration: ConfigurationModel,
}

impl FoundationModels {
    pub fn configuration_model(&self) -> &ConfigurationModel {
        &self.configuration
    }

    pub fn configuration_model_mut(&mut self) -> &mut ConfigurationModel {
        &mut self.configuration
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ConfigurationModel {
    secure_beacon: bool,
    default_ttl: u8,
    publish_period: u8,
}

impl ConfigurationModel {
    pub fn secure_beacon(&self) -> bool {
        self.secure_beacon
    }

    pub fn secure_beacon_mut(&mut self) -> &mut bool {
        &mut self.secure_beacon
    }

    pub fn default_ttl(&self) -> u8 {
        self.default_ttl
    }

    pub fn default_ttl_mut(&mut self) -> &mut u8 {
        &mut self.default_ttl
    }

    pub fn publish_period(&self) -> u8 {
        self.publish_period
    }

    pub fn publish_period_mut(&mut self) -> &mut u8 {
        &mut self.publish_period
    }

    pub fn publish_period_duration(&self) -> Option<Duration> {
        let steps = (self.publish_period & 0x3F) as u64;
        let res = (self.publish_period & 0xC0) >> 6;

        if steps == 0 {
            return None;
        }

        match res {
            0b00 => Some(Duration::from_millis(100 * steps)),
            0b01 => Some(Duration::from_secs(steps)),
            0b10 => Some(Duration::from_secs(10 * steps)),
            0b11 => Some(Duration::from_secs(600 * steps)),
            _ => None,
        }
    }
}

impl Default for ConfigurationModel {
    fn default() -> Self {
        Self {
            secure_beacon: true,
            default_ttl: 127,
            publish_period: 0,
        }
    }
}
