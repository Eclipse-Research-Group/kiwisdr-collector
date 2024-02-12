use kiwisdr::{KiwiSdrBuilder, KiwiSdrError, KiwiSdrSndClient};
use serde::Deserialize;
use url::Url;


#[derive(Debug, Deserialize, PartialEq, Eq)]
pub enum WebSdrScraperType {
    KiwiSDR
}

#[derive(Debug, Deserialize)]
pub struct WebSdrConfig {
    #[serde(rename = "type")]
    pub scraper_type: WebSdrScraperType,
    pub name: String,
    pub endpoint: String,
    pub password: Option<String>,
    pub frequency: i32,
    pub bandwidth: i32,

}

impl WebSdrConfig {
    pub async fn create_client(&self) -> Result<KiwiSdrSndClient, KiwiSdrError> {
        let endpoint = Url::parse(&self.endpoint).unwrap();
        KiwiSdrBuilder::new(endpoint)
            .with_name(&self.name)
            .with_password(self.password.clone().as_deref())
            .build_snd()
            .await
    }
}

enum RecorderError {
    SdrError(String),
    ExistsError
}

impl From<KiwiSdrError> for RecorderError {
    fn from(error: KiwiSdrError) -> Self {
        RecorderError::SdrError(format!("{:?}", error))
    }
}

struct SDRRecorder {
    config: WebSdrConfig,
    sdr: Option<KiwiSdrSndClient>,
}

impl SDRRecorder {
    pub fn new(config: WebSdrConfig) -> Self {
        Self {
            config: config,
            sdr: None
        }
    }

    pub async fn respawn(&mut self) -> Result<(), RecorderError> {
        if self.sdr.is_some() {
            return Err(RecorderError::ExistsError);
        }

        let kiwi = self.config.create_client().await?;

        self.sdr = Some(kiwi);

        Ok(())
    }
}