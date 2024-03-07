use url::Url;
use anyhow::Result;
use crate::sdr::kiwi::KiwiSdrBuilder;
use crate::sdr::kiwi::KiwiSdrSndClient;

pub mod kiwi;

pub struct AgcConfig {
    agc: bool,
    hang: bool,
    thresh: i16,
    slope: u8,
    decay: u32,
    man_gain: u8,
}

impl AgcConfig {
    pub fn new(agc: bool, hang: bool, thresh: i16, slope: u8, decay: u32, man_gain: u8) -> Self {
        Self { agc, hang, thresh, slope, decay, man_gain }
    }
    pub fn to_string(&self) -> String {
        format!(
            "SET agc={} hang={} thresh={} slope={} decay={} manGain={}",
            if self.agc { 1 } else { 0 }, 
            if self.hang { 1 } else { 0 },
             self.thresh, self.slope, self.decay, self.man_gain
        )
    }
}

pub struct AMTuning {
    pub bandwidth: i32,
    pub freq: f64,
}

pub struct GeneralTuning {
    pub low_cut: i32,
    pub high_cut: i32,
    pub freq: f64,
}

pub enum Station {
    AM(AMTuning),
    FM(GeneralTuning),
    LSB(GeneralTuning),
    USB(GeneralTuning),
}

impl Station {
    pub fn to_message(&self) -> String {
        match self {
            Station::AM(config) => {
                format!("SET mod=am low_cut={} high_cut={} freq={}",
                    (-config.bandwidth / 2) as i32,
                    (config.bandwidth / 2) as i32,
                    config.freq)
            },
            Station::FM(config) => {
                format!("SET mod=fm low_cut={} high_cut={} freq={}",
                    config.low_cut,
                    config.high_cut,
                    config.freq)
            },
            Station::LSB(config) => {
                format!("SET mod=lsb low_cut={} high_cut={} freq={}",
                    config.low_cut,
                    config.high_cut,
                    config.freq)
            },
            Station::USB(config) => {
                format!("SET mod=usb low_cut={} high_cut={} freq={}",
                    config.low_cut,
                    config.high_cut,
                    config.freq)
            },
        }
    }
}

pub struct SDRWrapper {
    client: Option<KiwiSdrSndClient>,
    password: Option<String>,
    endpoint: String,
    name: String,
    station: Station
}

impl SDRWrapper {
    pub fn new(endpoint: String, name: String, station: Station, password: Option<String>) -> SDRWrapper {
        SDRWrapper {
            client: None,
            password,
            endpoint,
            name,
            station
        }
    }

    pub async fn connect(&mut self) -> Result<()> {
        self.client = Some(KiwiSdrBuilder::new(Url::parse(self.endpoint.as_str())?)
            .with_name(&self.name)
            .with_password(self.password.clone().as_deref())
            .build_snd()
            .await.unwrap());
        Ok(())
    }

}

struct SDRCollector {

}