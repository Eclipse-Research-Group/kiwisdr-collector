use super::{AgcConfig, Station};
use anyhow::Result;
use byteorder::{BigEndian, ByteOrder, LittleEndian, ReadBytesExt};
use futures_util::stream::SplitSink;
use futures_util::{future, pin_mut, SinkExt, StreamExt};
use rand::Rng;
use simple_logger::SimpleLogger;
use std::io::BufWriter;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::{env, fs::File, io::Write, path::Path, sync::Arc};
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use url::Url;

pub fn add(left: usize, right: usize) -> usize {
    left + right
}

#[derive(Debug)]
pub enum KiwiSdrError {
    ConnectionError(String),
    SendError(String),
    ConnectionClosed,
    CannotBuild(String),
}

pub struct KiwiSdrBuilderConfig {
    pub endpoint: Url,
    pub password: Option<String>,
    pub name: Option<String>,
}

pub struct KiwiSdrBuilder {
    config: KiwiSdrBuilderConfig,
}

impl KiwiSdrBuilderConfig {
    pub fn login_message(&self) -> String {
        if self.password.is_some() {
            format!("SET auth t=kiwi p={}", self.password.as_ref().unwrap())
        } else {
            format!("SET auth t=kiwi p=#")
        }
    }
}

static SDR_COUNTER: AtomicU64 = AtomicU64::new(0);

impl KiwiSdrBuilder {
    pub fn new(endpoint: Url) -> Self {
        Self {
            config: KiwiSdrBuilderConfig {
                endpoint: endpoint,
                password: None,
                name: None,
            },
        }
    }

    pub fn with_password(mut self, password: Option<&str>) -> Self {
        self.config.password = password.map(|p| p.to_string());
        self
    }

    pub fn with_name(mut self, name: &str) -> Self {
        self.config.name = Some(name.to_string());
        self
    }

    pub async fn build_snd(self) -> Result<KiwiSdrSndClient, KiwiSdrError> {
        if self.config.name.is_none() {
            return Err(KiwiSdrError::CannotBuild("No name provided".to_string()));
        }
        KiwiSdrSndClient::from_builder(self.config).await
    }
}

#[derive(Clone)]
pub struct KiwiSdrStats {
    pub rssi: f32,
}

pub struct KiwiSdrSndClient {
    name: String,
    cancellation_token: tokio_util::sync::CancellationToken,
    stats: Arc<tokio::sync::Mutex<KiwiSdrStats>>,
    sample_rate: Arc<AtomicU32>,
    ready_notify: Arc<tokio::sync::Notify>,
    sound_rx: tokio::sync::mpsc::Receiver<Vec<i16>>,
    ws_write: Arc<
        tokio::sync::Mutex<
            SplitSink<
                tokio_tungstenite::WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
                Message,
            >,
        >,
    >,
}

impl KiwiSdrSndClient {
    async fn from_builder(config: KiwiSdrBuilderConfig) -> Result<Self, KiwiSdrError> {
        let endpoint = {
            let mut rng = rand::thread_rng();
            config
                .endpoint
                .join(format!("/kiwi/{}/SND", rng.gen_range(0..10000)).as_str())
                .unwrap()
        };
        let (ws_socket, _) = tokio_tungstenite::connect_async(endpoint.clone())
            .await
            .map_err(|e| KiwiSdrError::ConnectionError(format!("{}", e)))?;
        let (mut write, read) = ws_socket.split();

        // Login
        write
            .send(Message::Text(config.login_message()))
            .await
            .map_err(|e| KiwiSdrError::SendError(format!("{}", e)))?;

        let (sound_tx, sound_rx) = tokio::sync::mpsc::channel::<Vec<i16>>(3);
        let write = Arc::new(tokio::sync::Mutex::new(write));
        let notify = Arc::new(tokio::sync::Notify::new());
        let stats = Arc::new(tokio::sync::Mutex::new(KiwiSdrStats { rssi: 0.0 }));
        let sample_rate = Arc::new(AtomicU32::new(12000));
        let token = tokio_util::sync::CancellationToken::new();

        // Start listening task, move a copy of what we need
        let write_clone = write.clone();
        let notify_clone = notify.clone();
        let stats_clone = stats.clone();
        let sample_rate_clone = sample_rate.clone();
        let token_clone = token.clone();
        let name = config.name.clone().unwrap();
        tokio::spawn(async move {
            let write = write_clone;
            let stats = stats_clone;
            let notify = notify_clone;
            let sample_rate_atomic = sample_rate_clone;

            let endpoint = endpoint.clone();
            let sound_tx = sound_tx.clone();

            read.for_each(|msg| async {
                let msg = match msg {
                    Ok(msg) => msg,
                    Err(e) => {
                        log::error!("{}: Error: {}", name, e);
                        return ();
                    }
                };

                match msg {
                    Message::Ping(_) => {
                        log::debug!("Ping from {}", endpoint);
                    },
                    Message::Binary(data) => {
                        let code = String::from_utf8(data[..3].to_vec()).unwrap();
                        match code.as_str() {
                            "MSG" => {
                                let str = String::from_utf8(data[4..].to_vec()).unwrap();
                                log::debug!("{}: {}", endpoint, str[..std::cmp::min(30, str.len())].to_string());
                                if str.starts_with("audio_init") {
                                    let sample_rate = {
                                        let re = regex::Regex::new(r"audio_init=(\d+)\s+audio_rate=(\d+)\s+sample_rate=([\d.]+)").unwrap();
                                        let caps = re.captures(&str).ok_or("No match found").unwrap();

                                        let audio_init: i16 = caps[1].parse().unwrap();
                                        let audio_rate: f64 = caps[2].parse::<f64>().unwrap().ceil();
                                        let sample_rate: f64 = caps[3].parse().unwrap();
                                        audio_rate as u32
                                    };

                                    sample_rate_atomic.store(sample_rate, Ordering::Relaxed);

                                    let mut write = write.lock().await;
                                    write.send(Message::Text("SET AR OK in=12000 out=44100".to_string())).await.unwrap();
                                    write.send(Message::Text("SET squelch=0 param=0.00".to_string())).await.unwrap();
                                    notify.notify_one();
                                }
                            },
                            "SND" => {
                                let data = data[3..].to_vec();
                                let flags = data[0];
                                let seq = LittleEndian::read_u32(&data[1..5]);
                                let smeter = BigEndian::read_u16(&data[5..7]);

                                let rssi = 0.1 * smeter as f32 - 127.0;
                                {
                                    let mut stats = stats.lock().await;
                                    stats.rssi = rssi;
                                }

                                let data = data[7..].to_vec();
                                let mut output_vec = Vec::<i16>::with_capacity(data.len() / 2);

                                let mut cursor = std::io::Cursor::new(data);
                                while let Ok(b) = cursor.read_u16::<LittleEndian>() {
                                    output_vec.push(b as i16);
                                }

                                match sound_tx.send(output_vec).await {
                                    Ok(_) => {},
                                    Err(e) => {
                                        log::warn!("Sound buffer full!");
                                    }
                                }
                            }
                            _ => {}
                        }
                    },
                    Message::Close(_) => {
                        token_clone.cancel();
                        // log::warn!("{} closed the connection", endpoint);
                    },
                    _ => {}
                }
            }).await;
        });

        Ok(Self {
            name: config.name.unwrap(),
            sample_rate: sample_rate,
            stats: stats,
            ws_write: write,
            sound_rx: sound_rx,
            ready_notify: notify,
            cancellation_token: token,
        })
    }

    pub async fn configure_agc(&mut self, config: AgcConfig) -> Result<(), KiwiSdrError> {
        let write = self.ws_write.clone();
        let mut write = write.lock().await;
        write
            .send(Message::Text(config.to_string()))
            .await
            .map_err(|e| KiwiSdrError::ConnectionError(format!("{}", e)))?;
        Ok(())
    }

    pub async fn wait_for_start(&mut self) -> Result<(), KiwiSdrError> {
        self.ready_notify.clone().notified().await;
        Ok(())
    }

    pub async fn tune(&mut self, freq: Station) -> Result<(), KiwiSdrError> {
        let write = self.ws_write.clone();
        let mut write = write.lock().await;
        write
            .send(Message::Text(freq.to_message()))
            .await
            .map_err(|e| KiwiSdrError::ConnectionError(format!("{}", e)))?;
        Ok(())
    }

    pub async fn get_sound_data(&mut self) -> Result<Vec<i16>, KiwiSdrError> {
        match self.sound_rx.recv().await {
            Some(sound_data) => Ok(sound_data),
            None => Err(KiwiSdrError::ConnectionClosed),
        }
    }

    pub async fn set_compression(&mut self, compression: bool) -> Result<(), KiwiSdrError> {
        let write = self.ws_write.clone();
        let mut write = write.lock().await;
        write
            .send(Message::Text(format!(
                "SET compression={}",
                if compression { 1 } else { 0 }
            )))
            .await
            .map_err(|e| KiwiSdrError::ConnectionError(format!("{}", e)))?;
        Ok(())
    }

    pub async fn set_callsign(&mut self, callsign: &str) -> Result<(), KiwiSdrError> {
        let write = self.ws_write.clone();
        let mut write = write.lock().await;
        write
            .send(Message::Text(format!("SET ident_user={}", callsign)))
            .await
            .map_err(|e| KiwiSdrError::ConnectionError(format!("{}", e)))?;
        Ok(())
    }

    pub async fn set_location(&mut self, location: &str) -> Result<(), KiwiSdrError> {
        let write = self.ws_write.clone();
        let mut write = write.lock().await;
        write
            .send(Message::Text(format!("SET geoloc={}", location)))
            .await
            .map_err(|e| KiwiSdrError::ConnectionError(format!("{}", e)))?;
        Ok(())
    }

    pub async fn get_stats(&self) -> Result<KiwiSdrStats, KiwiSdrError> {
        let stats = self.stats.clone();
        let stats = stats.lock().await;
        Ok(stats.clone())
    }

    pub async fn send_keepalive(&mut self) -> Result<(), KiwiSdrError> {
        let write = self.ws_write.clone();
        let mut write = write.lock().await;
        write
            .send(Message::Text("SET keepalive".to_string()))
            .await
            .map_err(|e| KiwiSdrError::ConnectionError(format!("{}", e)))?;
        Ok(())
    }

    pub async fn get_sample_rate(&self) -> Result<u32, KiwiSdrError> {
        Ok(self.sample_rate.load(Ordering::Relaxed))
    }

    pub async fn wait_for_close(&mut self) {
        self.cancellation_token.cancelled().await
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancellation_token.is_cancelled()
    }
}
