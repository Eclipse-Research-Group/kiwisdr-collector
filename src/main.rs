use std::{collections::HashMap, path::Path};

use colored::Colorize;
use hound::{Sample, WavSpec, WavWriter};
use kiwisdr::{
    kiwi::{AMTuning, AgcConfig, Station},
    KiwiSdrBuilder, KiwiSdrError, KiwiSdrSndClient,
};
use serde::Deserialize;
use url::Url;

#[derive(Debug, Deserialize, PartialEq, Eq)]
enum WebSdrScraperType {
    KiwiSDR,
}

#[derive(Debug, Deserialize)]
struct WebSdrConfig {
    #[serde(rename = "type")]
    scraper_type: WebSdrScraperType,
    name: String,
    endpoint: String,
    password: Option<String>,
    frequency: i32,
    bandwidth: i32,
}

impl WebSdrConfig {
    async fn create_client(&self) -> Result<KiwiSdrSndClient, KiwiSdrError> {
        let endpoint = Url::parse(&self.endpoint).unwrap();
        KiwiSdrBuilder::new(endpoint)
            .with_name(&self.name)
            .with_password(self.password.clone().as_deref())
            .build_snd()
            .await
    }
}

#[derive(Debug, Deserialize)]
struct Config {
    data_dir: String,
    callsign: String,
    location: String,
    stations: Vec<WebSdrConfig>,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 10)]
async fn main() {
    simple_logger::init_with_level(log::Level::Debug).unwrap();

    let config_contents = std::fs::read_to_string("config.toml").unwrap();
    let config = toml::from_str::<Config>(&config_contents).unwrap();

    std::fs::create_dir_all(&config.data_dir).unwrap();

    log::info!("Found {} stations", config.stations.len());

    // Validate the stations
    for station in &config.stations {
        if station.scraper_type != WebSdrScraperType::KiwiSDR {
            log::error!("Unsupported scraper type: {:?}", station.scraper_type);
            return;
        }

        if Url::parse(&station.endpoint).is_err() {
            log::error!("Invalid URL: {:?}", station.endpoint);
            return;
        }
    }

    let mut map: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();

    for station in config.stations {
        log::info!(
            "Connecting to {} at {}...",
            station.name.green(),
            station.endpoint
        );
        let data_dir = config.data_dir.clone();
        let station_name = station.name.clone();
        let callsign = config.callsign.clone();
        let location = config.location.clone();

        let task = tokio::spawn(async move {
            let client = station.create_client().await;
            log::info!(
                "Connected to {} at {}",
                station.name.green(),
                station.endpoint
            );
            if let Ok(mut kiwi_sdr) = client {
                tokio::select! {
                    _ = kiwi_sdr.wait_for_start() => {},
                    _ = tokio::time::sleep(std::time::Duration::from_secs(6)) => {
                        log::error!("{}: Timed out waiting for start, goodbye.", station.name.green());
                        return;
                    }
                };

                let sample_rate = kiwi_sdr.get_sample_rate().await.unwrap();
                log::info!("{}: Sample rate: {} Hz", station.name.green(), sample_rate);

                log::info!(
                    "Tuning {} to {} kHz...",
                    station.name.green(),
                    station.frequency
                );
                kiwi_sdr
                    .tune(Station::AM(AMTuning {
                        bandwidth: station.bandwidth,
                        freq: station.frequency as f64,
                    }))
                    .await
                    .unwrap();

                kiwi_sdr.set_callsign(&callsign).await.unwrap();
                kiwi_sdr.set_location(&location).await.unwrap();

                kiwi_sdr
                    .configure_agc(AgcConfig::new(false, false, -20, 6, 1000, 10))
                    .await
                    .unwrap();
                kiwi_sdr.set_compression(false).await.unwrap();

                // TODO on close
                // TODO keep track of how many connected
                // TODO reconnecting

                let mut writer = WavWriter::create(
                    Path::new(&data_dir)
                        .join(format!("{}_{}.wav", station.name, station.frequency)),
                    WavSpec {
                        channels: 1,
                        sample_rate: sample_rate,
                        bits_per_sample: 16,
                        sample_format: hound::SampleFormat::Int,
                    },
                )
                .unwrap();

                let kiwi_sdr_arc = std::sync::Arc::new(tokio::sync::Mutex::new(kiwi_sdr));

                let kiwi_sdr_clone = kiwi_sdr_arc.clone();
                let station_name = station.name.clone();
                tokio::spawn(async move {
                    let kiwi_sdr = kiwi_sdr_clone;
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        let mut kiwi = kiwi_sdr.lock().await;
                        if kiwi.is_cancelled() {
                            // Nothing more to do
                            return;
                        }

                        log::debug!("{}: Sending keepalive", station_name.green());
                        kiwi.send_keepalive().await.unwrap();
                    }
                });

                let kiwi_sdr = kiwi_sdr_arc.clone();

                while let Ok(data) = kiwi_sdr.lock().await.get_sound_data().await {
                    let mut writer = writer.get_i16_writer(data.len() as u32);
                    for sample in data {
                        writer.write_sample(sample);
                    }
                    writer.flush().unwrap();
                }
            }
        });

        map.insert(station_name.clone(), task);
    }

    loop {
        let mut to_remove: Vec<String> = Vec::new();

        for (station_name, task) in map.iter() {
            if task.is_finished() {
                log::info!("{}: Finished", station_name.green());
                to_remove.push(station_name.clone());
            }
        }

        if !to_remove.is_empty() {
            for station_name in to_remove.drain(..) {
                map.remove(&station_name);
            }
            // log::info!("{} tasks running", map.len());
            if map.is_empty() {
                log::info!("No more tasks, goodbye.");
                return;
            }
        }
    }
}
