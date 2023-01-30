use russh::ChannelMsg;
use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use serde::de::Visitor;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tokio::sync::mpsc::unbounded_channel;


use uuid::Uuid;

use crate::session_manager::{Error, Shell, ShellBuffer, ShellCallback, ShellInfo, ShellToken};

pub(crate) type ShellsMap = HashMap<ShellToken, Arc<Shell>>;

impl Shell {
    pub async fn write(&self, data: &[u8]) -> Result<(), Error> {
        if let Some(sender) = self.sender.lock().await.as_mut() {
            sender.send(Vec::<u8>::from(data)).unwrap();
        } else {
            return Err(Error::disconnected());
        }
        return Ok(());
    }

    pub async fn resize(&self, cols: u16, rows: u16) -> Result<(), Error> {
        if let Some(ch) = self.channel.lock().await.as_mut() {
            ch.window_change(cols as u32, rows as u32, 0, 0).await?;
        } else {
            return Err(Error::disconnected());
        }
        self.parser.lock().unwrap().set_size(rows, cols);
        return Ok(());
    }

    pub async fn screen(&self, cols: u16, rows: u16) -> Result<ShellBuffer, Error> {
        let guard = self.parser.lock().unwrap();
        let screen = guard.screen();
        let mut rows: Vec<Vec<u8>> = screen.rows_formatted(0, cols).collect();
        if let Some(idx) = rows.iter().rposition(|row| !row.is_empty()) {
            rows = Vec::from(&rows[0..idx + 1]);
        } else {
            rows = Vec::new();
        }
        return Ok(ShellBuffer {
            rows,
            cursor: screen.cursor_position(),
        });
    }

    pub async fn close(&self) -> Result<(), Error> {
        if let Some(ch) = self.channel.lock().await.take() {
            ch.close().await?;
        }
        return Ok(());
    }

    pub(crate) async fn run<CB>(&self, cb: CB) -> Result<(), Error>
    where
        CB: ShellCallback + Send + 'static,
    {
        let (sender, mut receiver) = unbounded_channel::<Vec<u8>>();
        *self.sender.lock().await = Some(sender);
        let mut status: Option<u32> = None;
        let mut eof: bool = false;
        loop {
            tokio::select! {
                data = receiver.recv() => {
                    log::info!("Write {{ data: {:?} }}", data);
                    match data {
                        // TODO transform data for dumb shell
                        Some(data) => self.send(&data[..]).await?,
                        None => {
                            self.close().await?;
                            break;
                        }
                    }
                }
                result = self.wait() => {
                    match result? {
                        ChannelMsg::Data { data } => {
                            // TODO: process data for dumb shell
                            let sh_changed = self.process(data.as_ref());
                            cb.rx(0, data.as_ref());
                            if sh_changed {
                                cb.info(self.info());
                            }
                        }
                        ChannelMsg::ExtendedData { data, ext } => {
                            log::info!("ExtendedData {{ data: {:?}, ext: {} }}", data, ext);
                            // TODO: process data for dumb shell
                            if ext == 1 {
                                self.process(data.as_ref());
                                cb.rx(1, data.as_ref());
                            }
                        }
                        ChannelMsg::ExitStatus { exit_status } => {
                            status = Some(exit_status);
                            if eof {
                                break;
                            }
                        }
                        ChannelMsg::Eof => {
                            eof = true;
                            if status.is_some() {
                                break;
                            }
                        }
                        ChannelMsg::Close => log::info!("Channel:Close"),
                        e => log::info!("Channel:{:?}", e)
                    }
                }
            }
        }
        return Ok(());
    }

    pub fn info(&self) -> ShellInfo {
        return ShellInfo {
            token: self.token.clone(),
            title: self.title(),
            has_pty: self.has_pty,
            created_at: self.created_at,
        };
    }

    async fn activate(&self, cols: u16, rows: u16) -> Result<(), Error> {
        if self.sender.lock().await.is_some() {
            return Ok(());
        }
        if let Some(ch) = self.channel.lock().await.as_mut() {
            log::info!(
                "initializing {:?} with {cols} cols and {rows} rows",
                self.token
            );
        } else {
            return Err(Error::disconnected());
        }
        return Ok(());
    }

    async fn wait(&self) -> Result<ChannelMsg, Error> {
        return if let Some(ch) = self.channel.lock().await.as_mut() {
            let msg = ch.wait().await;
            msg.ok_or_else(|| Error::disconnected())
        } else {
            Err(Error::disconnected())
        };
    }

    async fn send(&self, data: &[u8]) -> Result<(), Error> {
        return if let Some(ch) = self.channel.lock().await.as_mut() {
            return Ok(ch.data(data).await?);
        } else {
            Err(Error::disconnected())
        };
    }

    fn process(&self, data: &[u8]) -> bool {
        if !self.has_pty {
            return false;
        }
        let mut parser = self.parser.lock().unwrap();
        let old = parser.screen().clone();
        parser.process(data);
        return !parser.screen().title_diff(&old).is_empty();
    }

    fn title(&self) -> String {
        let guard = self.parser.lock().unwrap();
        let title = guard.screen().title();
        if title.is_empty() {
            return self.def_title.clone();
        }
        return String::from(title);
    }
}

impl Serialize for ShellToken {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        return serializer.serialize_str(&format!("{}/{}", self.connection_id, self.channel_id));
    }
}

impl<'de> Deserialize<'de> for ShellToken {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        return deserializer.deserialize_string(ShellTokenVisitor);
    }
}

struct ShellTokenVisitor;

impl<'de> Visitor<'de> for ShellTokenVisitor {
    type Value = ShellToken;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("string")
    }

    // parse the version from the string
    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: std::error::Error,
    {
        let mut split = value.split('/');
        let first = split.next().unwrap();
        let second = split.next().unwrap();
        return Ok(ShellToken {
            connection_id: Uuid::from_str(first).unwrap(),
            channel_id: String::from(second),
        });
    }
}
